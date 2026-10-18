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
use std::sync::{Arc, PoisonError};
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

/// How long a tunnel must stay up before its end resets the backoff.
const STABLE_TUNNEL: Duration = Duration::from_secs(30);

/// How long a failed accept pauses `connect`'s listener.
const ACCEPT_ERROR_PAUSE: Duration = Duration::from_millis(100);

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
        let opened = tokio::time::Instant::now();
        let ended = match Tunnel::open(relay, protocol, token).await {
            Ok(tunnel) => {
                tracing::info!(relay = %relay.addr, "tunnel dialed");
                serve_tunnel(&tunnel, target, &tls).await
            }
            Err(err) => err,
        };
        if ended.is_auth_refused() {
            return Err(ended);
        }
        // `open` returns before the relay admits the tunnel, so only a
        // tunnel that stayed up resets the backoff: a relay at its cap, or
        // another `expose` reclaiming the route, backs off instead of
        // looping twice a second.
        if opened.elapsed() >= STABLE_TUNNEL {
            backoff = BACKOFF_BASE;
        }
        if ended.close_code() == Some(crate::tunnel::RECLAIMED_CODE) {
            tracing::warn!("another tunnel claimed this route; is a second `expose` running?");
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
            // An ACME TLS-ALPN-01 validation ends with its handshake; it is
            // never a consumer of the target (RFC 8737 §3).
            if accepted.get_ref().1.alpn_protocol() == Some(ACME_TLS_ALPN) {
                return Ok(());
            }
            forward_to(accepted, target).await
        }
    }
}

/// The TLS-ALPN-01 validation ALPN, spelled here so the forwarder needs no
/// `acme` feature to recognize (and not forward) a validation.
const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

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

/// How `connect` trusts the service at the far end of the stream.
#[derive(Debug, Clone)]
pub enum ServiceTrust {
    /// The service's certificate fingerprint ([`crate::pin`]), for the
    /// self-signed certificate `expose` keeps by default. The TLS server
    /// name is the route.
    Pin(String),
    /// A certificate a CA in `roots` issued for `name` (an ACME certificate,
    /// or any other public one), verified the way a browser would, so
    /// renewal never breaks it. `name` is also the TLS server name.
    Name {
        /// The name the certificate must cover.
        name: String,
        /// The CAs trusted to issue it.
        roots: Arc<rustls::RootCertStore>,
    },
    /// No TLS inside the stream: the service runs
    /// [`ServiceTls::Passthrough`] and its target speaks TLS itself.
    Passthrough,
}

/// Mozilla's root store, for [`ServiceTrust::Name`].
#[must_use]
pub fn web_roots() -> rustls::RootCertStore {
    rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    }
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
    /// How to trust the service inside the stream.
    pub service: ServiceTrust,
    /// Carry every local connection as a stream on one shared relay
    /// connection, for a relay whose protocol bridges every stream
    /// ([`crate::Protocol::consumer_streams`] above one). Otherwise each
    /// local connection dials its own.
    pub multiplex: bool,
}

/// Accept local connections on `listener` and carry each through the relay
/// to `route`, forever.
///
/// A failed accept (a connection reset before accept, the process out of
/// descriptors) is logged and retried after a short pause; it never drops
/// the forwards in flight.
pub async fn connect(listener: TcpListener, route: Route) {
    let relay = Arc::new(RelayLink::new(route));
    loop {
        let (local, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(err) => {
                tracing::warn!(%err, "accept failed");
                tokio::time::sleep(ACCEPT_ERROR_PAUSE).await;
                continue;
            }
        };
        let relay = Arc::clone(&relay);
        tokio::spawn(async move {
            if let Err(err) = relay.carry(local).await {
                tracing::warn!(%peer, route = %relay.route.name, %err, "forward failed");
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

/// One route's way through the relay: with [`Route::multiplex`], a cached
/// connection every forward opens its stream on, redialed once it dies.
#[derive(Debug)]
struct RelayLink {
    route: Route,
    shared: std::sync::Mutex<Option<(quinn::Endpoint, quinn::Connection)>>,
    /// Held while redialing, so one dial serves every waiting forward.
    dialing: tokio::sync::Mutex<()>,
}

/// A stream to the route and, when it has the connection to itself, the
/// connection to close once the stream is done.
type RouteStream = (
    tokio::io::Join<quinn::RecvStream, quinn::SendStream>,
    Option<(quinn::Endpoint, quinn::Connection)>,
);

impl RelayLink {
    fn new(route: Route) -> Self {
        Self {
            route,
            shared: std::sync::Mutex::new(None),
            dialing: tokio::sync::Mutex::new(()),
        }
    }

    /// A stream to the route: on the shared connection when multiplexing
    /// (dialing it if there is none, or it died), else on its own.
    async fn open(&self) -> Result<RouteStream, ConnectError> {
        let route = &self.route;
        if !route.multiplex {
            let (endpoint, conn, send, recv) =
                dial_route(&route.relay, &route.name, &route.alpn).await?;
            return Ok((tokio::io::join(recv, send), Some((endpoint, conn))));
        }
        // Waits while the relay's stream limit is reached.
        if let Some(conn) = self.live()
            && let Ok((send, recv)) = conn.open_bi().await
        {
            return Ok((tokio::io::join(recv, send), None));
        }
        let (send, recv) = self.redial().await?.open_bi().await?;
        Ok((tokio::io::join(recv, send), None))
    }

    /// The shared connection, while it is open.
    fn live(&self) -> Option<quinn::Connection> {
        let cached = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        cached
            .as_ref()
            .map(|(_, conn)| conn)
            .filter(|conn| conn.close_reason().is_none())
            .cloned()
    }

    /// A new shared connection. One redial at a time: forwards that found
    /// the old connection dead together share the first one's result.
    async fn redial(&self) -> Result<quinn::Connection, ConnectError> {
        let _dialing = self.dialing.lock().await;
        if let Some(conn) = self.live() {
            return Ok(conn);
        }
        let route = &self.route;
        let (endpoint, conn) =
            crate::connect::dial_route_connection(&route.relay, &route.name, &route.alpn).await?;
        *self.shared.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((endpoint, conn.clone()));
        Ok(conn)
    }

    /// One local connection, through the relay to the route.
    async fn carry(&self, mut local: TcpStream) -> Result<(), CarryError> {
        let _ = local.set_nodelay(true);
        let (stream, own) = self.open().await?;
        let (config, name) = match &self.route.service {
            ServiceTrust::Passthrough => {
                let mut stream = stream;
                tokio::io::copy_bidirectional(&mut local, &mut stream).await?;
                drain(stream, own).await;
                return Ok(());
            }
            ServiceTrust::Pin(pin) => (
                crate::pin::stream_client_config(pin)
                    .map_err(|err| std::io::Error::other(err.to_string()))?,
                self.route.name.clone(),
            ),
            ServiceTrust::Name { name, roots } => (named_client_config(roots)?, name.clone()),
        };
        let name = rustls::pki_types::ServerName::try_from(name)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;
        let mut session = tokio::time::timeout(
            TLS_HANDSHAKE_DEADLINE,
            tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, stream),
        )
        .await
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
        tokio::io::copy_bidirectional(&mut local, &mut session).await?;
        drain(session.into_inner().0, own).await;
        Ok(())
    }
}

/// A TLS 1.3 client config verifying the service against `roots`.
fn named_client_config(
    roots: &Arc<rustls::RootCertStore>,
) -> std::io::Result<rustls::ClientConfig> {
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .map_err(|err| std::io::Error::other(err.to_string()))?
    .with_root_certificates(Arc::clone(roots))
    .with_no_client_auth())
}

/// Let the relay acknowledge everything sent before the stream is dropped,
/// then, when the stream had its connection to itself, close that
/// connection (dropping it would discard undelivered stream data).
async fn drain(
    stream: tokio::io::Join<quinn::RecvStream, quinn::SendStream>,
    own: Option<(quinn::Endpoint, quinn::Connection)>,
) {
    let (_recv, send) = stream.into_inner();
    let _ = tokio::time::timeout(DRAIN_DEADLINE, send.stopped()).await;
    if let Some((endpoint, conn)) = own {
        conn.close(0u32.into(), b"done");
        let _ = tokio::time::timeout(DRAIN_DEADLINE, endpoint.wait_idle()).await;
    }
}
