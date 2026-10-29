//! QUIC dialer ([ADR-0007]): the outbound counterpart to the server's
//! `QuicListener`.
//!
//! Opens one bidirectional stream carrying the same length-prefixed phux
//! frames as UDS, after building the rustls config (TLS 1.3, caller-selected
//! ALPN, pin or loopback skip) and writing the optional ADR-0031 bearer
//! preamble (`len: u32 BE` + raw token) as the first stream bytes.
//!
//! **Tunnel tag.** A dial offering [`QUIC_RELAY_ALPN`] uses an initial
//! destination connection ID of [`TUNNEL_CID_TAG`] plus 16 random bytes. A
//! relay reads it off the first packet to pick the tunnel's bounded
//! flow-control config, which quinn fixes at accept time, before the ALPN is
//! known. The tag selects a config, never a role: admission is still by ALPN.
//!
//! [ADR-0007]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0007-mosh-class-transport-and-satellites.md

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
/// `docs/protocol.md#transports`), shared with the server listener and relay.
pub const AUTH_FAILED_CODE: u32 = quictun::tunnel::AUTH_FAILED_CODE;

/// The tag phux tunnel dials put on their initial destination connection ID:
/// `phxT`.
pub const TUNNEL_CID_TAG: quictun::CidTag = quictun::CidTag(*b"phxT");

/// A fresh tagged connection ID: the tag plus 16 CSPRNG bytes.
fn tunnel_cid() -> Result<quinn::ConnectionId, DialError> {
    TUNNEL_CID_TAG
        .fresh_cid()
        .map_err(|_| DialError::Connect("no randomness for the tunnel connection ID".to_owned()))
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
    /// An end-to-end TLS session to run inside the stream: a relayed dial
    /// whose relay terminates the outer TLS (ADR-0154 item 5). Set, only
    /// [`dial_stream`] dials it.
    pub inner: Option<InnerTls>,
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

/// [`dial`] offering an explicit ALPN, for legs that negotiate a distinct
/// protocol (the ADR-0051 connector passes [`QUIC_RELAY_ALPN`]).
pub async fn dial_with_alpn(d: &QuicDial, alpn: &[u8]) -> Result<QuicConnection, DialError> {
    dial_inner(d, alpn, d.identity.as_ref()).await
}

/// The server a relayed dial reaches through its relay, end to end.
///
/// How to trust it (its CA pin) and the workload identity to present to it.
/// The outer QUIC handshake trusts the relay and presents nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InnerTls {
    /// The server's certificate trust (its CA, ADR-0153).
    pub trust: CertTrust,
    /// The identity presented to the server.
    pub identity: TlsClientIdentity,
    /// The name offered in the inner handshake (the relay route).
    pub server_name: String,
}

/// The end-to-end session over one relayed QUIC stream.
pub type InnerStream =
    tokio_rustls::client::TlsStream<tokio::io::Join<quinn::RecvStream, quinn::SendStream>>;

/// What [`dial_stream`] established.
#[derive(Debug)]
pub enum DialedStream {
    /// The QUIC stream itself, its bearer preamble written.
    Plain {
        /// Client-to-server half.
        send: quinn::SendStream,
        /// Server-to-client half.
        recv: quinn::RecvStream,
    },
    /// The end-to-end session inside it, its bearer preamble written inside.
    Inner(Box<InnerStream>),
}

/// [`dial`], or the relayed dial for one carrying [`QuicDial::inner`].
///
/// The relayed dial is the outer handshake to the relay, then the end-to-end
/// session to the server inside the stream, with the bearer preamble written
/// inside it.
///
/// # Errors
///
/// As [`dial`]; a server presenting another CA than the inner pin is
/// [`DialError::AuthorityChanged`].
pub async fn dial_stream(
    d: &QuicDial,
) -> Result<(quinn::Endpoint, quinn::Connection, DialedStream), DialError> {
    let Some(inner) = &d.inner else {
        let (endpoint, conn, send, recv) = dial_inner(d, QUIC_ALPN, d.identity.as_ref()).await?;
        return Ok((endpoint, conn, DialedStream::Plain { send, recv }));
    };
    let (endpoint, conn, mut stream) = dial_relayed(d, inner, QUIC_ALPN).await?;
    if let Some(token) = &d.token {
        use tokio::io::AsyncWriteExt as _;
        let len = u32::try_from(token.len())
            .map_err(|_| DialError::Connect("pairing token too long".to_owned()))?;
        let written = async {
            stream.write_all(&len.to_be_bytes()).await?;
            stream.write_all(token).await?;
            stream.flush().await
        }
        .await;
        written
            .map_err(|err| write_lost(&conn, err, "write token inside the end-to-end session"))?;
    }
    Ok((endpoint, conn, DialedStream::Inner(Box::new(stream))))
}

/// The outer dial to the relay (no token, no identity), then the inner
/// handshake under `alpn`.
pub(crate) async fn dial_relayed(
    d: &QuicDial,
    inner: &InnerTls,
    alpn: &[u8],
) -> Result<(quinn::Endpoint, quinn::Connection, InnerStream), DialError> {
    let outer = QuicDial {
        token: None,
        inner: None,
        ..d.clone()
    };
    let (endpoint, conn, send, recv) =
        dial_inner(&outer, QUIC_ALPN, Some(&TlsClientIdentity::None)).await?;
    let (config, refusal) =
        crate::tls::client_config_reporting(&inner.trust, Some(&inner.identity), Some(alpn))?;
    let name = rustls::pki_types::ServerName::try_from(inner.server_name.clone())
        .map_err(|err| DialError::Connect(format!("invalid TLS server name: {err}")))?;
    let stream = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, tokio::io::join(recv, send))
        .await
        .map_err(|err| {
            crate::tls::authority_refusal(&refusal).unwrap_or_else(|| {
                conn.close_reason().map_or_else(
                    || DialError::Connect(format!("end-to-end TLS through the relay: {err}")),
                    |close| close_error(&close),
                )
            })
        })?;
    Ok((endpoint, conn, stream))
}

/// Handshake only, offering `alpn`, and open no stream.
///
/// For a leg whose peer opens the streams (a satellite's link to its hub,
/// ADR-0182). Presents [`QuicDial::identity`]; [`QuicDial::token`] is not
/// sent, since there is no stream to carry it.
///
/// # Errors
///
/// As [`dial`].
pub async fn connect_with_alpn(
    d: &QuicDial,
    alpn: &[u8],
) -> Result<(quinn::Endpoint, quinn::Connection), DialError> {
    handshake(d, alpn, d.identity.as_ref()).await
}

async fn dial_inner(
    d: &QuicDial,
    alpn: &[u8],
    identity: Option<&TlsClientIdentity>,
) -> Result<QuicConnection, DialError> {
    let (endpoint, conn) = handshake(d, alpn, identity).await?;
    let (mut send, recv) = conn
        .open_bi()
        .await
        .map_err(|err| DialError::Connect(format!("open QUIC stream: {err}")))?;

    if let Some(token) = &d.token {
        write_preamble(&conn, &mut send, token).await?;
    }

    Ok((endpoint, conn, send, recv))
}

/// Bind a client endpoint and complete the QUIC handshake under `alpn`.
async fn handshake(
    d: &QuicDial,
    alpn: &[u8],
    identity: Option<&TlsClientIdentity>,
) -> Result<(quinn::Endpoint, quinn::Connection), DialError> {
    if d.inner.is_some() {
        return Err(DialError::Connect(
            "a dial with an end-to-end session inside the stream goes through dial_stream"
                .to_owned(),
        ));
    }
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
    Ok((endpoint, conn))
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
pub(crate) fn client_config(
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
mod tests;
