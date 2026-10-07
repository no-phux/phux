//! QUIC dialer ([ADR-0007]): the outbound counterpart to the server's
//! `QuicListener`.
//!
//! Opens one bidirectional stream carrying the same length-prefixed phux
//! frames as UDS, after building the rustls config (TLS 1.3, caller-selected
//! ALPN, pin or loopback skip) and writing the optional ADR-0031 bearer
//! preamble (`len: u32 BE` + raw token) as the first stream bytes.
//!
//! **Tunnel tag.** A dial offering [`QUIC_RELAY_ALPN`] uses an initial
//! destination connection ID of [`TUNNEL_CID_PREFIX`] plus 16 random bytes. A
//! relay reads it off the first packet to pick the tunnel's bounded
//! flow-control config, which quinn fixes at accept time, before the ALPN is
//! known. The tag selects a config, never a role: admission is still by ALPN.
//!
//! [ADR-0007]: ../../../docs/adr/0007-mosh-class-transport-and-satellites.md

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use phux_protocol::policy::{QUIC_ALPN, QUIC_RELAY_ALPN};

use crate::DialError;
use crate::tls::{CertTrust, TlsClientIdentity};

/// Established QUIC endpoint, connection, and one bidirectional stream.
///
/// The endpoint is returned so the caller keeps its I/O driver alive and can
/// close cleanly on teardown.
pub type QuicConnection = (
    quinn::Endpoint,
    quinn::Connection,
    quinn::SendStream,
    quinn::RecvStream,
);

/// QUIC idle timeout, matched to the server's so a quiet consumer is not
/// reaped before the keep-alive fires.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep-alive interval, well under [`IDLE_TIMEOUT`] to hold NAT bindings.
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// QUIC application close code for a refused auth preamble (`AUTH_FAILED`,
/// `docs/spec/proto.md` §4.1), shared with the server listener and relay.
pub const AUTH_FAILED_CODE: u32 = 0x01;

/// First bytes of a tunnel dial's initial destination connection ID: `phxT`.
pub const TUNNEL_CID_PREFIX: [u8; 4] = *b"phxT";

/// Length of a tagged connection ID: QUIC's 20-byte maximum.
const TUNNEL_CID_LEN: usize = 20;

/// Whether an initial destination connection ID carries the tunnel tag.
#[must_use]
pub fn is_tunnel_cid(cid: &[u8]) -> bool {
    cid.len() == TUNNEL_CID_LEN && cid.starts_with(&TUNNEL_CID_PREFIX)
}

/// A fresh tagged connection ID: the prefix plus 16 CSPRNG bytes.
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
    /// TLS server name offered in SNI.
    pub server_name: String,
    /// Raw bearer-token bytes for the auth preamble, or `None` for an
    /// unauthenticated loopback listener (see [`parse_token_hex`]).
    pub token: Option<Vec<u8>>,
    /// How to trust the server's certificate.
    pub trust: CertTrust,
    /// The TLS client identity to present. `None` reads it from
    /// `PHUX_WORKLOAD_CERT` / `PHUX_WORKLOAD_KEY` (see
    /// [`crate::tls::client_config`]); `Some` is exactly this identity and
    /// never reads the environment, which is what a registry entry that
    /// enrolled a client certificate, or an embedder, supplies.
    pub identity: Option<TlsClientIdentity>,
}

/// Decode a `phux pair` pairing token (hex, surrounding whitespace allowed).
///
/// # Errors
///
/// Returns [`DialError::Connect`] when the token is not valid hex.
pub fn parse_token_hex(token: &str) -> Result<Vec<u8>, DialError> {
    hex::decode(token.trim())
        .map_err(|err| DialError::Connect(format!("pairing token is not valid hex: {err}")))
}

/// Connect with the production consumer ALPN ([`QUIC_ALPN`]).
///
/// Returns the established stream halves with the auth preamble written.
/// Presents [`QuicDial::identity`], or with none reads the optional workload
/// identity from the environment (see [`crate::tls::client_config`]).
///
/// # Errors
///
/// [`DialError::Unreachable`] when the handshake times out,
/// [`DialError::AuthRefused`] when the peer closes with `AUTH_FAILED`, and
/// [`DialError::Connect`] on any other failure.
pub async fn dial(d: &QuicDial) -> Result<QuicConnection, DialError> {
    dial_inner(d, QUIC_ALPN, d.identity.as_ref()).await
}

/// [`dial`] with an explicit TLS identity, overriding [`QuicDial::identity`];
/// never reads the environment.
pub async fn dial_with_identity(
    d: &QuicDial,
    identity: &TlsClientIdentity,
) -> Result<QuicConnection, DialError> {
    dial_inner(d, QUIC_ALPN, Some(identity)).await
}

/// [`dial`] offering an explicit ALPN, for legs that negotiate a distinct
/// protocol (the ADR-0051 connector passes [`QUIC_RELAY_ALPN`]).
pub async fn dial_with_alpn(d: &QuicDial, alpn: &[u8]) -> Result<QuicConnection, DialError> {
    dial_inner(d, alpn, d.identity.as_ref()).await
}

async fn dial_inner(
    d: &QuicDial,
    alpn: &[u8],
    identity: Option<&TlsClientIdentity>,
) -> Result<QuicConnection, DialError> {
    // A v4 client socket cannot reach a v6 listener and vice versa.
    let bind = if d.addr.is_ipv6() {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
    };
    let mut endpoint = quinn::Endpoint::client(bind)
        .map_err(|err| DialError::Connect(format!("bind QUIC client socket: {err}")))?;
    let (mut config, refusal) = client_config(&d.trust, identity, alpn)?;
    if alpn == QUIC_RELAY_ALPN {
        // One endpoint per dial, so one tagged ID serves its one connection.
        let cid = tunnel_cid()?;
        config.initial_dst_cid_provider(Arc::new(move || cid));
    }
    endpoint.set_default_client_config(config);

    let conn = endpoint
        .connect(d.addr, &d.server_name)
        .map_err(|err| DialError::Connect(format!("dial {}: {err}", d.addr)))?
        .await
        .map_err(|err| {
            crate::tls::authority_refusal(&refusal).unwrap_or_else(|| handshake_error(d.addr, &err))
        })?;

    let (mut send, recv) = conn
        .open_bi()
        .await
        .map_err(|err| DialError::Connect(format!("open QUIC stream: {err}")))?;

    if let Some(token) = &d.token {
        write_preamble(&conn, &mut send, token).await?;
    }

    Ok((endpoint, conn, send, recv))
}

/// Classify a failed handshake like [`close_error`], naming the address.
fn handshake_error(addr: SocketAddr, err: &quinn::ConnectionError) -> DialError {
    let msg = format!("QUIC handshake with {addr}: {err}");
    match close_error(err) {
        refused @ DialError::AuthRefused(_) => refused,
        DialError::Unreachable(_) => DialError::Unreachable(msg),
        _ => DialError::Connect(msg),
    }
}

/// Classify a QUIC connection close.
///
/// An application close with [`AUTH_FAILED_CODE`] and reason `unauthorized`
/// (or empty) is a credential refusal; a timeout is unreachable; everything
/// else, including a graceful close, is [`DialError::Connect`].
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

/// Write the auth preamble: `len: u32 BE` + raw token bytes.
///
/// A write that races the server's `AUTH_FAILED` close is classified from
/// the close reason; quinn's stream error alone reads `connection lost`.
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

/// The quinn client config: rustls TLS 1.3 with `alpn`, the trust policy's
/// verifier, and the server's idle / keep-alive timings.
fn client_config(
    trust: &CertTrust,
    identity: Option<&TlsClientIdentity>,
    alpn: &[u8],
) -> Result<(quinn::ClientConfig, crate::tls::AuthorityRefusal), DialError> {
    let (crypto, refusal) = crate::tls::client_config_reporting(trust, identity, Some(alpn))?;
    let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
        .map_err(|err| DialError::Connect(format!("build QUIC crypto: {err}")))?;
    let mut config = quinn::ClientConfig::new(Arc::new(quic_crypto));

    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    if let Ok(idle) = IDLE_TIMEOUT.try_into() {
        transport.max_idle_timeout(Some(idle));
    }
    config.transport_config(Arc::new(transport));
    Ok((config, refusal))
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
        assert_eq!(
            parse_token_hex(&format!("  {hexed}\n")).expect("trimmed"),
            raw
        );
        assert!(parse_token_hex("nothex!!").is_err());
    }

    #[test]
    fn handshake_errors_classify_timeout_as_unreachable() {
        let addr: SocketAddr = "127.0.0.1:4433".parse().expect("addr");
        let err = handshake_error(addr, &quinn::ConnectionError::TimedOut);
        assert!(matches!(err, DialError::Unreachable(_)), "got {err:?}");
        assert_eq!(
            err.to_string(),
            "transport connect error: QUIC handshake with 127.0.0.1:4433: timed out"
        );
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
    fn only_auth_failed_closes_are_credential_refusal() {
        for reason in [&b"unauthorized"[..], b""] {
            let err = close_error(&application_close(AUTH_FAILED_CODE, reason));
            assert!(
                matches!(err, DialError::AuthRefused(ref r) if r == "unauthorized"),
                "got {err:?}"
            );
        }
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
    }

    #[test]
    fn tunnel_cids_are_tagged_full_length_and_random() {
        let first = tunnel_cid().expect("tagged cid");
        let second = tunnel_cid().expect("tagged cid");
        assert!(is_tunnel_cid(&first) && is_tunnel_cid(&second));
        assert_ne!(
            first[TUNNEL_CID_PREFIX.len()..],
            second[TUNNEL_CID_PREFIX.len()..]
        );

        let mut tagged = [0x42_u8; TUNNEL_CID_LEN];
        tagged[..4].copy_from_slice(&TUNNEL_CID_PREFIX);
        assert!(is_tunnel_cid(&tagged));
        assert!(!is_tunnel_cid(&[0x42_u8; TUNNEL_CID_LEN]));
        assert!(!is_tunnel_cid(&TUNNEL_CID_PREFIX));
        assert!(!is_tunnel_cid(&tagged[..8]));
    }

    /// Dial a loopback peer that reads the bearer preamble and then closes
    /// with `code` / `reason`; return how the client classifies the close.
    async fn dial_and_get_closed(code: u32, reason: &'static [u8]) -> DialError {
        let dir = tempfile::tempdir().expect("tempdir");
        let endpoint = crate::testing::quic_server(dir.path(), QUIC_ALPN);
        let addr = endpoint.local_addr().expect("local");
        let server = tokio::spawn(async move {
            let conn = endpoint
                .accept()
                .await
                .expect("incoming")
                .await
                .expect("handshake");
            let (_send, mut recv) = conn.accept_bi().await.expect("stream");
            let mut token = [0u8; 4 + 32];
            recv.read_exact(&mut token).await.expect("preamble");
            conn.close(code.into(), reason);
            conn.closed().await;
        });

        let dialed = dial(&QuicDial {
            addr,
            server_name: "localhost".to_owned(),
            token: Some(vec![0xAB; 32]),
            trust: CertTrust::SkipVerify,
            identity: None,
        })
        .await;
        let classified = match dialed {
            Err(err) => err,
            // The write can finish before the close arrives; the close
            // reason then comes from the connection, not the stream error.
            Ok((_endpoint, conn, _send, mut recv)) => {
                let mut buf = [0u8; 1];
                let _ = recv.read(&mut buf).await;
                close_error(&conn.closed().await)
            }
        };
        server.await.expect("server");
        classified
    }

    #[tokio::test]
    async fn refused_token_surfaces_auth_close_not_connection_lost() {
        let classified = dial_and_get_closed(AUTH_FAILED_CODE, b"unauthorized").await;
        assert!(
            matches!(classified, DialError::AuthRefused(ref reason) if reason == "unauthorized"),
            "got {classified:?}"
        );
        assert!(!classified.to_string().contains("connection lost"));
    }

    /// workload-auth §3 over QUIC: a client requiring paired authority fails
    /// the handshake against a listener that does not request a client
    /// certificate, so no bearer preamble and no phux frame is ever sent.
    #[tokio::test]
    async fn require_paired_refuses_a_quic_listener_that_did_not_ask() {
        let dir = tempfile::tempdir().expect("tempdir");
        let endpoint = crate::testing::quic_server(dir.path(), QUIC_ALPN);
        let addr = endpoint.local_addr().expect("local");
        let server = tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("incoming");
            incoming.await.is_ok()
        });
        let (certificate, private_key) = (dir.path().join("client.pem"), dir.path().join("c.key"));
        crate::cert::ensure_self_signed(&certificate, &private_key).expect("client pair");

        let dialed = dial_with_identity(
            &QuicDial {
                addr,
                server_name: "localhost".to_owned(),
                token: Some(vec![0xAB; 32]),
                trust: CertTrust::Pinned(
                    crate::cert::cert_fingerprint(&dir.path().join("cert.pem")).expect("pin"),
                ),
                identity: None,
            },
            &TlsClientIdentity::RequirePaired {
                certificate,
                private_key,
            },
        )
        .await;
        let Err(err) = dialed else {
            panic!("a listener that did not ask for the certificate was accepted");
        };
        assert!(
            err.to_string().contains(crate::tls::DOWNGRADE_REFUSED),
            "got {err:?}"
        );
        assert!(
            !server.await.expect("server"),
            "the server never completes the handshake"
        );
    }

    #[tokio::test]
    async fn graceful_close_is_not_credential_refusal() {
        let classified = dial_and_get_closed(0, b"bye").await;
        assert!(
            !matches!(classified, DialError::AuthRefused(_)),
            "got {classified:?}"
        );
    }
}
