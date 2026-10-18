//! Forwarding TCP through a relay (feature `forward`): what the `quictun`
//! binary's `expose` and `connect` run.
//!
//! - [`expose`] holds a service's tunnel and connects each bridged consumer
//!   to a local TCP target, redialing the relay when the tunnel drops.
//! - [`connect`] listens on a local TCP port and carries each accepted
//!   connection through the relay to a route.
//!
//! By default the service end terminates TLS ([`ServiceTls::Terminate`])
//! and `connect` pins its certificate, so even a plaintext TCP service
//! crosses the relay as ciphertext. With a public certificate the same
//! service also answers browsers arriving through the relay's TCP ingress.
//! [`ServiceTls::Passthrough`] forwards the stream raw, for a target that
//! speaks TLS itself. Every forward propagates half-close, as TCP does.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};

use crate::Protocol;
use crate::connect::{ConnectError, RelayAddr, Tunnel, dial_route};

/// How long a bridged stream has to complete its TLS handshake.
const TLS_HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// First redial delay after a tunnel drops; doubles up to [`BACKOFF_CAP`].
const BACKOFF_BASE: Duration = Duration::from_millis(500);

/// Longest redial delay.
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// How long a finished forward waits for the relay to acknowledge its last
/// bytes, and for its close to go out.
const DRAIN_DEADLINE: Duration = Duration::from_secs(2);

/// What the service end does with each bridged stream.
#[derive(Clone)]
pub enum ServiceTls {
    /// Terminate TLS with this config and forward plaintext to the target.
    Terminate(Arc<rustls::ServerConfig>),
    /// Forward the stream to the target unchanged.
    Passthrough,
}

impl std::fmt::Debug for ServiceTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Terminate(_) => "Terminate",
            Self::Passthrough => "Passthrough",
        })
    }
}

/// A TLS 1.3 server config over the PEM pair at `cert_path` / `key_path`,
/// offering `alpns` (none for a raw TCP service).
///
/// # Errors
///
/// The pair could not be read, or rustls refused it.
pub fn service_tls_config(
    cert_path: &Path,
    key_path: &Path,
    alpns: Vec<Vec<u8>>,
) -> Result<rustls::ServerConfig, ServiceTlsError> {
    let certs = crate::cert::load_certs(cert_path)?;
    let key = crate::cert::load_key(key_path)?;
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])?
    .with_no_client_auth()
    .with_single_cert(certs, key)?;
    config.alpn_protocols = alpns;
    Ok(config)
}

/// Why a service TLS config could not be built.
#[derive(Debug, thiserror::Error)]
pub enum ServiceTlsError {
    /// The certificate or key could not be read.
    #[error(transparent)]
    Cert(#[from] crate::cert::CertError),
    /// rustls refused the material.
    #[error("rustls: {0}")]
    Rustls(#[from] rustls::Error),
}

/// Hold a tunnel for `token`'s route and connect every bridged consumer to
/// `target`, redialing the relay with capped backoff whenever the tunnel
/// drops.
///
/// # Errors
///
/// Returns only when the relay refuses the token: redialing cannot fix that.
pub async fn expose(
    relay: &RelayAddr,
    protocol: &Protocol,
    token: &[u8],
    target: SocketAddr,
    tls: ServiceTls,
) -> Result<(), ConnectError> {
    let mut backoff = BACKOFF_BASE;
    loop {
        let ended = match Tunnel::open(relay, protocol, token).await {
            Ok(tunnel) => {
                tracing::info!(relay = %relay.addr, "tunnel up");
                backoff = BACKOFF_BASE;
                serve_tunnel(&tunnel, target, &tls).await
            }
            Err(err) => err,
        };
        if ended.is_auth_refused() {
            return Err(ended);
        }
        tracing::warn!(err = %ended, retry_in = ?backoff, "tunnel down; redialing");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_CAP);
    }
}

/// Serve one tunnel until it closes: each bridged consumer runs as its own
/// task. Returns why the tunnel ended.
pub async fn serve_tunnel(tunnel: &Tunnel, target: SocketAddr, tls: &ServiceTls) -> ConnectError {
    loop {
        let (send, recv) = match tunnel.accept().await {
            Ok(streams) => streams,
            Err(err) => return err,
        };
        let tls = tls.clone();
        tokio::spawn(async move {
            if let Err(err) = serve_consumer(tokio::io::join(recv, send), target, tls).await {
                tracing::debug!(%err, "consumer forward ended with an error");
            }
        });
    }
}

/// One bridged consumer: terminate TLS (or not), then forward to `target`.
async fn serve_consumer<S>(stream: S, target: SocketAddr, tls: ServiceTls) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match tls {
        ServiceTls::Passthrough => forward_to(stream, target).await,
        ServiceTls::Terminate(config) => {
            let accepted = tokio::time::timeout(
                TLS_HANDSHAKE_DEADLINE,
                tokio_rustls::TlsAcceptor::from(config).accept(stream),
            )
            .await
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
            forward_to(accepted, target).await
        }
    }
}

/// Connect to `target` and copy both ways, each direction's end of stream
/// forwarded as a shutdown.
async fn forward_to<S>(mut stream: S, target: SocketAddr) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut upstream = TcpStream::connect(target).await?;
    let _ = upstream.set_nodelay(true);
    tokio::io::copy_bidirectional(&mut stream, &mut upstream).await?;
    Ok(())
}

/// Where [`connect`] carries each local connection.
#[derive(Debug, Clone)]
pub struct Route {
    /// The relay and its pin.
    pub relay: RelayAddr,
    /// The route's name.
    pub name: String,
    /// The consumer ALPN to offer the relay.
    pub alpn: Vec<u8>,
    /// The service's certificate fingerprint, pinned in the TLS session
    /// inside the stream; `None` forwards the stream raw (the service runs
    /// [`ServiceTls::Passthrough`] and its target speaks TLS itself).
    pub service_pin: Option<String>,
}

/// Accept local connections on `listener` and carry each through the relay
/// to `route`, until the listener fails.
///
/// # Errors
///
/// Accepting on the listener failed.
pub async fn connect(listener: TcpListener, route: Route) -> std::io::Result<()> {
    let route = Arc::new(route);
    loop {
        let (local, peer) = listener.accept().await?;
        let route = Arc::clone(&route);
        tokio::spawn(async move {
            if let Err(err) = carry(local, &route).await {
                tracing::warn!(%peer, route = %route.name, %err, "forward failed");
            }
        });
    }
}

/// Why one local connection could not be carried to its route.
#[derive(Debug, thiserror::Error)]
pub enum CarryError {
    /// Reaching the route through the relay failed.
    #[error(transparent)]
    Relay(#[from] ConnectError),
    /// The TLS session with the service failed, or copying did.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// One local connection, through the relay to the route.
async fn carry(mut local: TcpStream, route: &Route) -> Result<(), CarryError> {
    let _ = local.set_nodelay(true);
    let (endpoint, conn, send, recv) = dial_route(&route.relay, &route.name, &route.alpn).await?;
    let stream = tokio::io::join(recv, send);
    let Some(pin) = &route.service_pin else {
        let mut stream = stream;
        tokio::io::copy_bidirectional(&mut local, &mut stream).await?;
        drain(stream, &conn, &endpoint).await;
        return Ok(());
    };
    let config = crate::pin::stream_client_config(pin)
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    let name = rustls::pki_types::ServerName::try_from(route.name.clone())
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;
    let mut session = tokio::time::timeout(
        TLS_HANDSHAKE_DEADLINE,
        tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, stream),
    )
    .await
    .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
    tokio::io::copy_bidirectional(&mut local, &mut session).await?;
    drain(session.into_inner().0, &conn, &endpoint).await;
    Ok(())
}

/// Let the relay acknowledge everything sent before the connection drops
/// (dropping it discards undelivered stream data), then close it.
async fn drain(
    stream: tokio::io::Join<quinn::RecvStream, quinn::SendStream>,
    conn: &quinn::Connection,
    endpoint: &quinn::Endpoint,
) {
    let (_recv, send) = stream.into_inner();
    let _ = tokio::time::timeout(DRAIN_DEADLINE, send.stopped()).await;
    conn.close(0u32.into(), b"done");
    let _ = tokio::time::timeout(DRAIN_DEADLINE, endpoint.wait_idle()).await;
}
