//! QUIC dialer (`phux-y8v6`, [ADR-0007]) — the outbound counterpart to the
//! server's `QuicListener` (`phux-server::transport::quic`).
//!
//! The dialer opens exactly **one** bidirectional QUIC stream to a
//! `phux server --quic` listener and carries the identical length-prefixed phux
//! frames (`docs/spec/proto.md` §5) the UDS path does; only the byte stream
//! underneath differs. This module owns the QUIC-specific establishment —
//! building the rustls client config (TLS 1.3 + a caller-selected ALPN; the
//! production consumer ALPN by default, while the ADR-0051 relay-connector
//! leg selects its own via [`dial_with_alpn`]), verifying the
//! server certificate (a fingerprint **pin** for routable hosts, or a loopback
//! **skip** for local dev), connecting, and writing the optional bearer-token
//! preamble — and hands back the raw quinn stream halves. The framing itself
//! stays with the callers (`phux-client::attach::connection`, the server hub's
//! link supervisor).
//!
//! **Auth.** TLS 1.3 is intrinsic to QUIC, so confidentiality is never
//! optional. For *authentication* of routable consumers the dialer mirrors the
//! server's bearer-token model (ADR-0031): it writes a length-prefixed token
//! (`len: u32 BE` + raw token bytes) as the very first bytes of the stream,
//! ahead of any phux frame. On a loopback listener no preamble is sent and
//! frames start immediately.
//!
//! **Tunnel tag.** A dial offering [`QUIC_RELAY_ALPN`] — a connector's
//! tunnel to a relay — uses an initial destination connection ID that starts
//! with [`TUNNEL_CID_PREFIX`] and carries 16 random bytes after it. A relay
//! reads that ID off the first packet, before it accepts, and gives the
//! tunnel its bounded flow-control config there: quinn fixes a connection's
//! per-stream windows at accept time, while the ALPN that decides the
//! connection's role is only known after the handshake. The tag selects a
//! config, never a role — the relay still admits by ALPN, so a consumer that
//! copies the tag only shrinks its own receive window.
//!
//! [ADR-0007]: ../../../docs/adr/0007-mosh-class-transport-and-satellites.md

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use phux_protocol::policy::{QUIC_ALPN, QUIC_RELAY_ALPN};

use crate::DialError;
use crate::tls::{CertTrust, TlsClientIdentity};

/// Established QUIC endpoint, connection, and one bidirectional stream.
pub type QuicConnection = (
    quinn::Endpoint,
    quinn::Connection,
    quinn::SendStream,
    quinn::RecvStream,
);

/// QUIC idle timeout, matched to the server's `IDLE_TIMEOUT` so a quiet but
/// attached consumer is not reaped before the keep-alive fires.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep-alive interval, comfortably under [`IDLE_TIMEOUT`] so a quiet consumer
/// (no keystrokes, no output) holds its connection open across NATs.
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// QUIC application close code for a connection refused at the auth
/// preamble. Same `0x01` / `AUTH_FAILED` the server listener and relay
/// emit (`docs/spec/proto.md` §4.1).
pub const AUTH_FAILED_CODE: u32 = 0x01;

/// First bytes of the initial destination connection ID a tunnel dial uses
/// (see the module docs): ASCII `phxT`.
pub const TUNNEL_CID_PREFIX: [u8; 4] = *b"phxT";

/// Length of a tagged connection ID: QUIC's maximum, 20 bytes — the prefix
/// plus 16 random bytes, twice the 8 unpredictable bytes RFC 9000 §7.2
/// requires of a client's initial destination connection ID.
const TUNNEL_CID_LEN: usize = 20;

/// Whether an initial destination connection ID carries the tunnel tag.
#[must_use]
pub fn is_tunnel_cid(cid: &[u8]) -> bool {
    cid.len() == TUNNEL_CID_LEN && cid.starts_with(&TUNNEL_CID_PREFIX)
}

/// A fresh tagged connection ID: [`TUNNEL_CID_PREFIX`] followed by 16 bytes
/// from rustls' `ring` CSPRNG (the provider this crate's TLS already uses).
fn tunnel_cid() -> Result<quinn::ConnectionId, DialError> {
    let mut bytes = [0_u8; TUNNEL_CID_LEN];
    bytes[..TUNNEL_CID_PREFIX.len()].copy_from_slice(&TUNNEL_CID_PREFIX);
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut bytes[TUNNEL_CID_PREFIX.len()..])
        .map_err(|_| DialError::Connect("no randomness for the tunnel connection ID".to_owned()))?;
    Ok(quinn::ConnectionId::new(&bytes))
}

/// Everything the dialer needs to reach a `phux server --quic` listener.
#[derive(Debug, Clone)]
pub struct QuicDial {
    /// The listener's `HOST:PORT`.
    pub addr: SocketAddr,
    /// TLS server name offered in SNI / used for certificate name matching.
    /// The server's self-signed cert carries `localhost` / `127.0.0.1` / `::1`
    /// SANs; a fingerprint pin does not rely on name matching, but a valid
    /// name keeps the handshake conventional.
    pub server_name: String,
    /// Raw bearer-token bytes for the auth preamble, or `None` for an
    /// unauthenticated (loopback) listener. Callers hex-decode the
    /// `phux pair` token into these raw bytes (see [`parse_token_hex`]).
    pub token: Option<Vec<u8>>,
    /// How to trust the server's certificate.
    pub trust: CertTrust,
}

/// Decode a `phux pair` pairing token (hex) into the raw bytes the QUIC auth
/// preamble carries.
///
/// # Errors
///
/// Returns [`DialError::Connect`] when the token is not valid hex.
pub fn parse_token_hex(token: &str) -> Result<Vec<u8>, DialError> {
    hex::decode(token.trim())
        .map_err(|err| DialError::Connect(format!("pairing token is not valid hex: {err}")))
}

/// Connect to the QUIC listener and return the established bidi-stream
/// halves, the auth preamble already written.
///
/// Offers the production consumer ALPN ([`QUIC_ALPN`]); every consumer and
/// hub-satellite dial goes through here. Legs that negotiate a different
/// protocol id (the ADR-0051 relay-connector tunnel) use [`dial_with_alpn`].
///
/// The quinn [`Endpoint`](quinn::Endpoint) and
/// [`Connection`](quinn::Connection) are returned alongside so the caller
/// can keep the endpoint's I/O driver alive for the connection's lifetime and
/// issue a clean `CONNECTION_CLOSE` on teardown (rather than leaving the server
/// to reap an abandoned connection at the idle timeout).
///
/// # Errors
///
/// Returns [`DialError::Unreachable`] when the handshake times out (nothing
/// answered), [`DialError::AuthRefused`] when the peer application-closes
/// with `AUTH_FAILED` after the token preamble, and [`DialError::Connect`]
/// on any other bind, handshake, certificate, or preamble failure.
pub async fn dial(d: &QuicDial) -> Result<QuicConnection, DialError> {
    dial_with_alpn(d, QUIC_ALPN).await
}

/// Connect with explicit TLS identity configuration and the production ALPN.
///
/// Unlike [`dial`], this path never reads `PHUX_WORKLOAD_CERT` or
/// `PHUX_WORKLOAD_KEY` from the process environment.
pub async fn dial_with_identity(
    d: &QuicDial,
    identity: &TlsClientIdentity,
) -> Result<QuicConnection, DialError> {
    dial_with_alpn_and_identity(d, QUIC_ALPN, identity).await
}

/// Connect to a QUIC listener offering an explicit ALPN, and return the
/// established bidi-stream halves, the auth preamble already written.
///
/// QUIC mandates ALPN, so the parameter is non-optional. Ordinary consumers
/// use [`dial`], which offers the production ALPN; pass a different token
/// only for a leg that deliberately negotiates a distinct protocol — the
/// ADR-0051 dial-out connector leg passes
/// `phux_protocol::policy::QUIC_RELAY_ALPN` so a relay can tell its tunnel
/// apart from consumer connections at the handshake, never from the bytes.
///
/// The quinn [`Endpoint`](quinn::Endpoint) and
/// [`Connection`](quinn::Connection) are returned alongside so the caller
/// can keep the endpoint's I/O driver alive for the connection's lifetime and
/// issue a clean `CONNECTION_CLOSE` on teardown (rather than leaving the server
/// to reap an abandoned connection at the idle timeout).
///
/// # Errors
///
/// Returns [`DialError::Unreachable`] when the handshake times out (nothing
/// answered), [`DialError::AuthRefused`] when the peer application-closes
/// with `AUTH_FAILED` after the token preamble, and [`DialError::Connect`]
/// on any other bind, handshake, certificate, or preamble failure.
pub async fn dial_with_alpn(d: &QuicDial, alpn: &[u8]) -> Result<QuicConnection, DialError> {
    dial_with_alpn_inner(d, alpn, None).await
}

/// Connect with an explicit ALPN and explicit TLS client identity.
///
/// This is the environment-independent counterpart to [`dial_with_alpn`].
pub async fn dial_with_alpn_and_identity(
    d: &QuicDial,
    alpn: &[u8],
    identity: &TlsClientIdentity,
) -> Result<QuicConnection, DialError> {
    dial_with_alpn_inner(d, alpn, Some(identity)).await
}

async fn dial_with_alpn_inner(
    d: &QuicDial,
    alpn: &[u8],
    identity: Option<&TlsClientIdentity>,
) -> Result<QuicConnection, DialError> {
    // Bind an ephemeral client UDP socket in the target's address family — a
    // v4 client socket cannot reach a v6 listener and vice versa.
    let bind = if d.addr.is_ipv6() {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
    };
    let mut endpoint = quinn::Endpoint::client(bind)
        .map_err(|err| DialError::Connect(format!("bind QUIC client socket: {err}")))?;
    let mut config = client_config(&d.trust, identity, alpn)?;
    if alpn == QUIC_RELAY_ALPN {
        // One endpoint and config per dial, so one tagged ID serves the one
        // connection this endpoint makes.
        let cid = tunnel_cid()?;
        config.initial_dst_cid_provider(Arc::new(move || cid));
    }
    endpoint.set_default_client_config(config);

    let conn = endpoint
        .connect(d.addr, &d.server_name)
        .map_err(|err| DialError::Connect(format!("dial {}: {err}", d.addr)))?
        .await
        .map_err(|err| handshake_error(d.addr, &err))?;

    let (mut send, recv) = conn
        .open_bi()
        .await
        .map_err(|err| DialError::Connect(format!("open QUIC stream: {err}")))?;

    if let Some(token) = &d.token {
        write_preamble(&conn, &mut send, token).await?;
    }

    Ok((endpoint, conn, send, recv))
}

/// Classify a failed QUIC handshake: `TimedOut` is the UDP analogue of
/// refused/no-route (nothing answered), an `AUTH_FAILED` application close
/// is a credential refusal, and everything else — including TLS alerts
/// from a certificate-pin mismatch — stays `Connect`.
fn handshake_error(addr: SocketAddr, err: &quinn::ConnectionError) -> DialError {
    match err {
        quinn::ConnectionError::ApplicationClosed(close) if is_auth_failed(close) => {
            close_error(err)
        }
        quinn::ConnectionError::TimedOut => {
            DialError::Unreachable(format!("QUIC handshake with {addr}: {err}"))
        }
        _ => DialError::Connect(format!("QUIC handshake with {addr}: {err}")),
    }
}

/// Classify a QUIC connection close.
///
/// An application close with [`AUTH_FAILED_CODE`] and reason `unauthorized`
/// (or empty) is a credential refusal — revoked, unknown, or missing
/// pairing token — not a lost path. Other closes, including idle timeout
/// and a graceful `CONNECTION_CLOSE`, stay [`DialError::Connect`] or
/// [`DialError::Unreachable`].
#[must_use]
pub fn close_error(err: &quinn::ConnectionError) -> DialError {
    match err {
        quinn::ConnectionError::ApplicationClosed(close) if is_auth_failed(close) => {
            DialError::AuthRefused(auth_refused_reason(close))
        }
        quinn::ConnectionError::TimedOut => DialError::Unreachable(err.to_string()),
        _ => DialError::Connect(err.to_string()),
    }
}

fn is_auth_failed(close: &quinn::ApplicationClose) -> bool {
    close.error_code.into_inner() == u64::from(AUTH_FAILED_CODE)
        && (close.reason.is_empty() || close.reason.as_ref() == b"unauthorized")
}

fn auth_refused_reason(close: &quinn::ApplicationClose) -> String {
    if close.reason.is_empty() {
        "unauthorized".to_owned()
    } else {
        String::from_utf8_lossy(&close.reason).into_owned()
    }
}

/// Write the auth preamble: `len: u32 BE` + raw token bytes (ADR-0031 parity
/// with the WebSocket `Authorization: Bearer` header).
///
/// A write that races the server's `AUTH_FAILED` close is classified as
/// credential refusal, not a generic I/O failure: quinn's stream error
/// displays as `connection lost` and would otherwise drop the close code.
async fn write_preamble(
    conn: &quinn::Connection,
    send: &mut quinn::SendStream,
    token: &[u8],
) -> Result<(), DialError> {
    let len = u32::try_from(token.len())
        .map_err(|_| DialError::Connect("pairing token too long".to_owned()))?;
    send.write_all(&len.to_be_bytes())
        .await
        .map_err(|err| write_lost(conn, err, "write token preamble"))?;
    send.write_all(token)
        .await
        .map_err(|err| write_lost(conn, err, "write token"))?;
    if let Some(err) = conn.close_reason() {
        return Err(close_error(&err));
    }
    Ok(())
}

fn write_lost(conn: &quinn::Connection, err: impl std::fmt::Display, what: &str) -> DialError {
    conn.close_reason().map_or_else(
        || DialError::Connect(format!("{what}: {err}")),
        |close| close_error(&close),
    )
}

/// Build the quinn client config: rustls TLS 1.3 with the given ALPN, the
/// chosen certificate verifier, and a transport config matching the server's
/// idle / keep-alive timings.
fn client_config(
    trust: &CertTrust,
    identity: Option<&TlsClientIdentity>,
    alpn: &[u8],
) -> Result<quinn::ClientConfig, DialError> {
    let crypto = match identity {
        Some(identity) => crate::tls::client_config_with_identity(trust, identity, Some(alpn))?,
        None => crate::tls::client_config(trust, Some(alpn))?,
    };
    let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
        .map_err(|err| DialError::Connect(format!("build QUIC crypto: {err}")))?;
    let mut config = quinn::ClientConfig::new(Arc::new(quic_crypto));

    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    if let Ok(idle) = IDLE_TIMEOUT.try_into() {
        transport.max_idle_timeout(Some(idle));
    }
    config.transport_config(Arc::new(transport));
    Ok(config)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn parse_token_hex_roundtrips_and_rejects_garbage() {
        let raw = [0xde, 0xad, 0xbe, 0xef];
        let hexed = hex::encode(raw);
        assert_eq!(parse_token_hex(&hexed).expect("valid hex"), raw);
        // Surrounding whitespace is tolerated (copy-paste from `phux pair`).
        assert_eq!(
            parse_token_hex(&format!("  {hexed}\n")).expect("trimmed"),
            raw
        );
        assert!(parse_token_hex("nothex!!").is_err());
    }

    #[test]
    fn handshake_timeout_classifies_unreachable() {
        let addr: SocketAddr = "127.0.0.1:4433".parse().expect("addr");
        let err = handshake_error(addr, &quinn::ConnectionError::TimedOut);
        assert!(matches!(err, DialError::Unreachable(_)), "got {err:?}");
        assert_eq!(
            err.to_string(),
            "transport connect error: QUIC handshake with 127.0.0.1:4433: timed out"
        );
    }

    #[test]
    fn handshake_non_timeout_stays_connect() {
        let addr: SocketAddr = "127.0.0.1:4433".parse().expect("addr");
        let err = handshake_error(addr, &quinn::ConnectionError::VersionMismatch);
        assert!(matches!(err, DialError::Connect(_)), "got {err:?}");
    }

    fn application_close(code: u32, reason: &'static [u8]) -> quinn::ConnectionError {
        quinn::ConnectionError::ApplicationClosed(quinn::ApplicationClose {
            error_code: quinn::VarInt::from_u32(code),
            reason: reason.into(),
        })
    }

    #[test]
    fn auth_failed_close_is_credential_refusal_not_loss() {
        let err = close_error(&application_close(AUTH_FAILED_CODE, b"unauthorized"));
        assert!(
            matches!(err, DialError::AuthRefused(ref reason) if reason == "unauthorized"),
            "got {err:?}"
        );
        assert!(
            !err.to_string().contains("connection lost"),
            "revoked token must not look like a dropped path: {err}"
        );
        let empty = close_error(&application_close(AUTH_FAILED_CODE, b""));
        assert!(
            matches!(empty, DialError::AuthRefused(ref reason) if reason == "unauthorized"),
            "got {empty:?}"
        );
    }

    #[test]
    fn non_auth_closes_are_not_credential_refusal() {
        for err in [
            application_close(0, b"bye"),
            application_close(AUTH_FAILED_CODE, b"stream timeout"),
            quinn::ConnectionError::TimedOut,
            quinn::ConnectionError::Reset,
            quinn::ConnectionError::LocallyClosed,
            quinn::ConnectionError::VersionMismatch,
        ] {
            let classified = close_error(&err);
            assert!(
                !matches!(classified, DialError::AuthRefused(_)),
                "{err:?} became {classified:?}"
            );
        }
        assert!(matches!(
            close_error(&quinn::ConnectionError::TimedOut),
            DialError::Unreachable(_)
        ));
    }

    #[test]
    fn tunnel_cids_are_tagged_full_length_and_random() {
        let first = tunnel_cid().expect("tagged cid");
        let second = tunnel_cid().expect("tagged cid");
        assert_eq!(first.len(), TUNNEL_CID_LEN);
        assert!(is_tunnel_cid(&first) && is_tunnel_cid(&second));
        assert_ne!(
            first[TUNNEL_CID_PREFIX.len()..],
            second[TUNNEL_CID_PREFIX.len()..],
            "the 16 bytes after the prefix are fresh per dial"
        );
    }

    #[test]
    fn only_a_full_length_prefixed_cid_is_a_tunnel() {
        let mut tagged = [0x42_u8; TUNNEL_CID_LEN];
        tagged[..4].copy_from_slice(&TUNNEL_CID_PREFIX);
        assert!(is_tunnel_cid(&tagged));
        // quinn's own default: 20 random bytes, untagged.
        assert!(!is_tunnel_cid(&[0x42_u8; TUNNEL_CID_LEN]));
        // The prefix alone, or at another length, is not the tag.
        assert!(!is_tunnel_cid(&TUNNEL_CID_PREFIX));
        assert!(!is_tunnel_cid(&tagged[..8]));
    }

    #[test]
    fn client_config_accepts_arbitrary_alpn() {
        // quinn's ClientConfig is not introspectable, so this is a smoke
        // test that a non-default ALPN builds a config at all; real ALPN
        // negotiation on the relay leg is covered by the relay connector
        // integration test (crates/phux-server/tests/federation/relay_connector_spike.rs).
        client_config(&CertTrust::SkipVerify, None, b"phux-relay/1")
            .expect("a non-default ALPN builds a client config");
    }

    #[test]
    fn client_config_accepts_explicit_no_identity() {
        client_config(
            &CertTrust::SkipVerify,
            Some(&TlsClientIdentity::None),
            QUIC_ALPN,
        )
        .expect("explicitly identity-free QUIC config");
    }

    /// A loopback QUIC peer that completes the TLS handshake, reads the
    /// bearer preamble, then application-closes the way the real listener
    /// refuses a revoked token (`AUTH_FAILED` / `unauthorized`).
    fn refuse_after_preamble() -> (tempfile::TempDir, quinn::Endpoint, SocketAddr) {
        use rustls::pki_types::pem::PemObject as _;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};

        let dir = tempfile::tempdir().expect("tempdir");
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        crate::cert::ensure_self_signed(&cert_path, &key_path).expect("cert");
        let certs = CertificateDer::pem_file_iter(&cert_path)
            .expect("cert pem")
            .collect::<Result<Vec<_>, _>>()
            .expect("certs");
        let key = PrivateKeyDer::from_pem_file(&key_path).expect("key");
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("tls13")
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("server cert");
        tls.alpn_protocols = vec![QUIC_ALPN.to_vec()];
        let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).expect("quic crypto");
        let server_config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        let endpoint = quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().expect("addr"))
            .expect("bind");
        let addr = endpoint.local_addr().expect("local");
        (dir, endpoint, addr)
    }

    #[tokio::test]
    async fn refused_token_surfaces_auth_close_not_connection_lost() {
        let (_dir, endpoint, addr) = refuse_after_preamble();
        let server = tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("incoming");
            let conn = incoming.await.expect("handshake");
            let (_send, mut recv) = conn.accept_bi().await.expect("stream");
            let mut header = [0u8; 4];
            recv.read_exact(&mut header).await.expect("len");
            let len = usize::try_from(u32::from_be_bytes(header)).expect("len");
            let mut token = vec![0u8; len];
            recv.read_exact(&mut token).await.expect("token");
            conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
            conn.closed().await;
        });

        let dialed = super::dial(&QuicDial {
            addr,
            server_name: "localhost".to_owned(),
            token: Some(vec![0xAB; 32]),
            trust: CertTrust::SkipVerify,
        })
        .await;
        let classified = match dialed {
            Err(err) => err,
            Ok((_endpoint, conn, _send, mut recv)) => {
                // The historical native-tunnel bug: write_all finishes
                // before the application close arrives, so the first
                // inbound read fails with quinn's Display "connection lost"
                // while close_reason still carries AUTH_FAILED.
                let mut buf = [0u8; 1];
                let read_err = recv.read(&mut buf).await.expect_err("refused");
                assert!(
                    read_err.to_string().contains("connection lost"),
                    "the raw stream error is what used to leak: {read_err}"
                );
                close_error(&conn.closed().await)
            }
        };
        assert!(
            matches!(classified, DialError::AuthRefused(ref reason) if reason == "unauthorized"),
            "got {classified:?}"
        );
        assert!(
            !classified.to_string().contains("connection lost"),
            "revoked token must not look like a dropped path: {classified}"
        );
        server.await.expect("server");
    }

    #[tokio::test]
    async fn graceful_close_is_not_credential_refusal() {
        let (_dir, endpoint, addr) = refuse_after_preamble();
        let server = tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("incoming");
            let conn = incoming.await.expect("handshake");
            let (_send, mut recv) = conn.accept_bi().await.expect("stream");
            let mut header = [0u8; 4];
            recv.read_exact(&mut header).await.expect("len");
            let len = usize::try_from(u32::from_be_bytes(header)).expect("len");
            let mut token = vec![0u8; len];
            recv.read_exact(&mut token).await.expect("token");
            conn.close(0u32.into(), b"bye");
            conn.closed().await;
        });

        let classified = match super::dial(&QuicDial {
            addr,
            server_name: "localhost".to_owned(),
            token: Some(vec![0xAB; 32]),
            trust: CertTrust::SkipVerify,
        })
        .await
        {
            Err(err) => err,
            Ok((_endpoint, conn, _send, mut recv)) => {
                let mut buf = [0u8; 1];
                let _ = recv.read(&mut buf).await;
                close_error(&conn.closed().await)
            }
        };
        assert!(
            !matches!(classified, DialError::AuthRefused(_)),
            "a non-auth close must stay generic, got {classified:?}"
        );
        server.await.expect("server");
    }
}
