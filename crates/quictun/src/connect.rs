//! The two client legs: a service's [`Tunnel`] to the relay, and a
//! consumer's [`dial_route`] through it.
//!
//! Both pin the relay's leaf ([`crate::pin`]). Neither runs anything inside
//! the bridged stream: a service and its consumers should run their own TLS
//! session there, so the relay forwards only ciphertext.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use crate::Protocol;
use crate::tunnel::{AUTH_FAILED_CODE, encode_preamble};

/// QUIC idle timeout, matching the relay's.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep-alive interval, well under [`IDLE_TIMEOUT`] to hold NAT bindings.
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// Why a client leg could not be established.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// Binding the client socket failed.
    #[error("bind QUIC client socket: {0}")]
    Bind(#[source] std::io::Error),
    /// The TLS config could not be built, or the server name is invalid.
    #[error("tls: {0}")]
    Tls(String),
    /// quinn refused to start the connection.
    #[error("connect: {0}")]
    Connect(#[from] quinn::ConnectError),
    /// The handshake or the connection failed; a refusal arrives here as an
    /// application close carrying one of the [`crate::tunnel`] codes.
    #[error("connection: {0}")]
    Connection(#[from] quinn::ConnectionError),
    /// Writing the preamble failed.
    #[error("write: {0}")]
    Write(#[from] quinn::WriteError),
    /// The token is longer than [`crate::tunnel::MAX_TOKEN_PREAMBLE`].
    #[error("route token too long")]
    TokenTooLong,
    /// The OS random source failed while tagging the connection ID.
    #[error("os random source unavailable")]
    Random,
}

impl ConnectError {
    /// Whether the relay refused the token ([`AUTH_FAILED_CODE`]).
    #[must_use]
    pub fn is_auth_refused(&self) -> bool {
        self.close_code() == Some(AUTH_FAILED_CODE)
    }

    /// The application close code the relay refused with, if it did.
    #[must_use]
    pub fn close_code(&self) -> Option<u32> {
        let Self::Connection(quinn::ConnectionError::ApplicationClosed(close)) = self else {
            return None;
        };
        u32::try_from(close.error_code.into_inner()).ok()
    }
}

/// Where a client leg goes and how it trusts the relay.
#[derive(Debug, Clone)]
pub struct RelayAddr {
    /// The relay's UDP address.
    pub addr: SocketAddr,
    /// The relay's leaf fingerprint ([`crate::pin::fingerprint`]).
    pub pin: String,
}

/// A service's live tunnel: the relay opens one stream on it per consumer.
#[derive(Debug)]
pub struct Tunnel {
    /// Kept so the I/O driver lives as long as the tunnel.
    endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    /// Stream 0, held open: finishing it is not a violation, but the relay
    /// reads nothing more from it and a service has nothing more to say.
    _stream0: (quinn::SendStream, quinn::RecvStream),
}

impl Tunnel {
    /// Dial `relay` as a tunnel for whichever route `token` is enrolled for.
    ///
    /// The relay may close a token it does not know only after reading the
    /// preamble, so a refusal can surface from [`Self::accept`] rather than
    /// here; [`ConnectError::is_auth_refused`] names it either way.
    ///
    /// # Errors
    ///
    /// The dial, handshake, or preamble write failed.
    pub async fn open(
        relay: &RelayAddr,
        protocol: &Protocol,
        token: &[u8],
    ) -> Result<Self, ConnectError> {
        let preamble = encode_preamble(token).ok_or(ConnectError::TokenTooLong)?;
        let mut config = quic_config(&relay.pin, &protocol.tunnel_alpn)?;
        // One endpoint per dial, so one tagged ID serves its one connection.
        let cid = protocol
            .cid_tag
            .fresh_cid()
            .map_err(|_| ConnectError::Random)?;
        config.initial_dst_cid_provider(Arc::new(move || cid));
        // The relay ignores a tunnel's server name; any valid one will do.
        let (endpoint, conn) = connect(relay.addr, "tunnel", config).await?;
        let (mut send, recv) = conn.open_bi().await?;
        send.write_all(&preamble).await?;
        Ok(Self {
            endpoint,
            conn,
            _stream0: (send, recv),
        })
    }

    /// The next consumer the relay bridged onto this tunnel.
    ///
    /// # Errors
    ///
    /// The tunnel closed: a refused token, a newer claim on the route
    /// ([`crate::tunnel::RECLAIMED_CODE`]), or a lost path. Redial.
    pub async fn accept(&self) -> Result<(quinn::SendStream, quinn::RecvStream), ConnectError> {
        Ok(self.conn.accept_bi().await?)
    }

    /// The tunnel connection.
    #[must_use]
    pub const fn connection(&self) -> &quinn::Connection {
        &self.conn
    }

    /// Close the tunnel and wait briefly for the close to reach the relay.
    pub async fn close(self) {
        self.conn.close(0u32.into(), b"tunnel closed");
        let _ = tokio::time::timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
    }
}

/// A consumer's connection through the relay to `route`, and its one
/// stream: the relay bridges only a consumer's first stream.
pub type Consumer = (
    quinn::Endpoint,
    quinn::Connection,
    quinn::SendStream,
    quinn::RecvStream,
);

/// Dial `relay` as a consumer of `route`, offering `alpn` (one of the
/// relay's consumer ALPNs), and open the stream the relay bridges.
///
/// An unknown route fails the handshake; an enrolled route with no live
/// tunnel closes with [`crate::tunnel::ROUTE_OFFLINE_CODE`], which surfaces
/// on the first read or write of the stream. quinn sends a stream's opening
/// lazily, so the relay sees it only once the consumer writes.
///
/// # Errors
///
/// The dial or the handshake failed.
pub async fn dial_route(
    relay: &RelayAddr,
    route: &str,
    alpn: &[u8],
) -> Result<Consumer, ConnectError> {
    let config = quic_config(&relay.pin, alpn)?;
    let (endpoint, conn) = connect(relay.addr, route, config).await?;
    let (send, recv) = conn.open_bi().await?;
    Ok((endpoint, conn, send, recv))
}

/// The quinn client config: the pinned TLS config plus the relay's idle and
/// keep-alive timings.
fn quic_config(pin: &str, alpn: &[u8]) -> Result<quinn::ClientConfig, ConnectError> {
    let crypto =
        crate::pin::client_config(pin, alpn).map_err(|err| ConnectError::Tls(err.to_string()))?;
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
        .map_err(|err| ConnectError::Tls(err.to_string()))?;
    let mut config = quinn::ClientConfig::new(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    if let Ok(idle) = IDLE_TIMEOUT.try_into() {
        transport.max_idle_timeout(Some(idle));
    }
    config.transport_config(Arc::new(transport));
    Ok(config)
}

/// Bind a client endpoint of `addr`'s family and complete the handshake.
async fn connect(
    addr: SocketAddr,
    server_name: &str,
    config: quinn::ClientConfig,
) -> Result<(quinn::Endpoint, quinn::Connection), ConnectError> {
    // A v4 client socket cannot reach a v6 relay and vice versa.
    let bind = if addr.is_ipv6() {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
    } else {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
    };
    let mut endpoint = quinn::Endpoint::client(bind).map_err(ConnectError::Bind)?;
    endpoint.set_default_client_config(config);
    let conn = endpoint.connect(addr, server_name)?.await?;
    Ok((endpoint, conn))
}
