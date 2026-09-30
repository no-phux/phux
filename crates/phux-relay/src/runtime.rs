//! Relay runtime: the QUIC endpoint, its accept loop, tunnel admission,
//! and consumer bridging. One current-thread runtime, one task per
//! connection; per-connection failures are logged and never tear down the
//! endpoint.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use phux_dial::quic::is_tunnel_cid;
use phux_dial::window::{SendWindow, TrackedSend};
use phux_protocol::policy::{QUIC_ALPN, QUIC_RELAY_ALPN};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::registry::TunnelRegistry;
use crate::splice::splice;
use crate::tokens::{CachedRouteTokens, RouteTokenStore};
use crate::{
    AUTH_FAILED_CODE, OVER_CAP_CODE, PROTOCOL_VIOLATION_CODE, ROUTE_OFFLINE_CODE, RelayError, tls,
};

/// Default connection cap (`--max-conns`). Over-cap connections complete
/// their handshake and are closed with [`crate::OVER_CAP_CODE`].
pub const DEFAULT_MAX_CONNS: usize = 64;

/// Handshakes in flight per connection slot. A handshake takes no connection
/// slot until it completes, so peers that start one and stall cannot lock
/// out connectors and consumers; they only fill this separate, larger pool.
const HANDSHAKES_PER_SLOT: usize = 4;

/// How long a peer has to complete the QUIC/TLS handshake.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// Connections one source address may have between its first packet and
/// its admission.
///
/// Admission is the handshake, then a connector's auth preamble or a
/// consumer's first stream. Address-validated and unvalidated connections
/// are counted separately. A source past its unvalidated share must answer a
/// Retry, so spoofing a victim's address cannot use up the victim's share;
/// past its validated share it is refused. One address therefore holds at
/// most twice this many of the handshake pool, never all of it. Tests
/// shorten it via [`RelayRuntime::with_handshakes_per_source`].
pub const DEFAULT_HANDSHAKES_PER_SOURCE: usize = 16;

/// QUIC idle timeout, matching the server listener and phux-dial.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep-alive interval, well under [`IDLE_TIMEOUT`].
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// Auth-preamble size bound, mirroring the server QUIC listener's.
const MAX_TOKEN_PREAMBLE: usize = 256;

/// How long a connector has to present its stream-0 auth preamble; bounds a
/// slow-loris on the tunnel leg. Tests shorten it via
/// [`RelayRuntime::with_preamble_deadline`].
const PREAMBLE_DEADLINE: Duration = Duration::from_secs(5);

/// How long an admitted consumer has to open its stream (and the relay the
/// matching tunnel stream). Without it a handshake-only consumer would hold
/// its cap permit forever, since keep-alives stop QUIC from reaping it.
const CONSUMER_STREAM_DEADLINE: Duration = Duration::from_secs(5);

/// How long shutdown waits for close frames to drain before returning.
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(2);

/// Per-stream receive window a connector tunnel grants the server: one
/// stream per bridged consumer.
///
/// Credit the relay has granted but not forwarded is lag. Held small, a slow
/// consumer blocks the server's writer quickly so its output pump resyncs the
/// consumer instead of queueing seconds of output. Only tagged tunnels get it
/// (see [`phux_dial::quic::TUNNEL_CID_PREFIX`]): consumers keep quinn's
/// defaults so uploads stay fast, and untagged legacy tunnels are logged.
/// Being per stream, one stalled consumer never holds back another.
///
/// This caps each consumer at one window per server-to-relay round trip
/// (about 5 Mbit/s at 100 ms); 64 KiB was measured to carry realistic floods
/// without resyncs where 16 and 32 KiB did not.
const TUNNEL_STREAM_RECEIVE_WINDOW: u32 = 64 * 1024;

/// The QUIC transport config for relay connections, with
/// [`TUNNEL_STREAM_RECEIVE_WINDOW`] for a `tunnel`.
fn transport_config(tunnel: bool) -> quinn::TransportConfig {
    let mut transport = quinn::TransportConfig::default();
    if let Ok(idle) = quinn::IdleTimeout::try_from(IDLE_TIMEOUT) {
        transport.max_idle_timeout(Some(idle));
    }
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    if tunnel {
        transport.stream_receive_window(quinn::VarInt::from_u32(TUNNEL_STREAM_RECEIVE_WINDOW));
    }
    transport
}

/// Everything the relay needs to run.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// Address the QUIC endpoint binds. Always explicit — there is no
    /// default listen address, so accidental exposure requires typing it.
    pub listen: SocketAddr,
    /// PEM certificate path (provisioned self-signed when missing).
    pub cert_path: PathBuf,
    /// PEM private-key path (provisioned alongside the certificate).
    pub key_path: PathBuf,
    /// Route-token store path (`<64-hex> <route>` lines).
    pub tokens_path: PathBuf,
    /// Connection cap; see [`DEFAULT_MAX_CONNS`].
    pub max_conns: usize,
}

impl RelayConfig {
    /// A config listening on `listen` with the fixed XDG state-dir paths
    /// and the default connection cap.
    #[must_use]
    pub fn new(listen: SocketAddr) -> Self {
        Self {
            listen,
            cert_path: crate::default_relay_cert_path(),
            key_path: crate::default_relay_key_path(),
            tokens_path: crate::default_relay_tokens_path(),
            max_conns: DEFAULT_MAX_CONNS,
        }
    }
}

/// The relay's run loop: [`Self::run_async`], or [`Self::bind`] then
/// [`BoundRelay::serve`] to learn the resolved listen address first.
#[derive(Debug)]
pub struct RelayRuntime {
    config: RelayConfig,
    preamble_deadline: Duration,
    handshakes_per_source: usize,
}

impl RelayRuntime {
    /// Wrap a config, ready to run.
    #[must_use]
    pub const fn new(config: RelayConfig) -> Self {
        Self {
            config,
            preamble_deadline: PREAMBLE_DEADLINE,
            handshakes_per_source: DEFAULT_HANDSHAKES_PER_SOURCE,
        }
    }

    /// Override the per-source admission share (default
    /// [`DEFAULT_HANDSHAKES_PER_SOURCE`]), so tests can fill it quickly.
    #[must_use]
    pub const fn with_handshakes_per_source(mut self, share: usize) -> Self {
        self.handshakes_per_source = share;
        self
    }

    /// Override the tunnel auth-preamble deadline (default 5s), so tests can
    /// wait a stalled preamble out quickly.
    #[must_use]
    pub const fn with_preamble_deadline(mut self, deadline: Duration) -> Self {
        self.preamble_deadline = deadline;
        self
    }

    /// Run the relay until `shutdown` resolves or the endpoint closes.
    ///
    /// The token store is re-read whenever it changed, so `phux relay pair`
    /// and line deletion take effect without a restart.
    pub async fn run_async(self, shutdown: impl Future<Output = ()>) -> Result<(), RelayError> {
        self.bind()?.serve(shutdown).await
    }

    /// Validate the state files, build the endpoint, and bind the socket
    /// without serving yet, so the caller can learn the resolved address.
    /// Must be called within a tokio runtime context.
    pub fn bind(self) -> Result<BoundRelay, RelayError> {
        let config = self.config;
        let preamble_deadline = self.preamble_deadline;
        // Fail-fast validation load; per-connection lookups re-read it
        // whenever the file changes.
        let store = RouteTokenStore::load(&config.tokens_path)?;
        let tokens = Arc::new(CachedRouteTokens::new(config.tokens_path.clone()));
        tls::ensure_self_signed(&config.cert_path, &config.key_path)?;
        let tls_config =
            tls::server_config(&config.cert_path, &config.key_path, Arc::clone(&tokens))?;
        let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls_config)
            .map_err(|err| RelayError::Rustls(rustls::Error::General(err.to_string())))?;
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        // Same crypto, bounded per-stream window, chosen per connection.
        let mut tunnel_config = server_config.clone();
        server_config.transport_config(Arc::new(transport_config(false)));
        tunnel_config.transport_config(Arc::new(transport_config(true)));

        let endpoint = quinn::Endpoint::server(server_config, config.listen)?;
        let local_addr = endpoint.local_addr()?;
        Ok(BoundRelay {
            endpoint,
            tunnel_config: Arc::new(tunnel_config),
            local_addr,
            routes: store.len(),
            tokens,
            max_conns: config.max_conns,
            preamble_deadline,
            handshakes_per_source: self.handshakes_per_source,
        })
    }
}

/// A relay whose endpoint is bound but not yet serving — the output of
/// [`RelayRuntime::bind`], consumed by [`Self::serve`].
#[derive(Debug)]
pub struct BoundRelay {
    endpoint: quinn::Endpoint,
    /// The server config a tagged tunnel is accepted with: the endpoint's
    /// crypto plus [`TUNNEL_STREAM_RECEIVE_WINDOW`].
    tunnel_config: Arc<quinn::ServerConfig>,
    local_addr: SocketAddr,
    routes: usize,
    tokens: Arc<CachedRouteTokens>,
    max_conns: usize,
    preamble_deadline: Duration,
    handshakes_per_source: usize,
}

impl BoundRelay {
    /// The endpoint's resolved bound address: when the config asked for
    /// port 0, this carries the OS-assigned port.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Serve the bound relay until `shutdown` resolves or the endpoint
    /// closes. Per-connection failures are logged and never propagate.
    pub async fn serve(self, shutdown: impl Future<Output = ()>) -> Result<(), RelayError> {
        tracing::info!(
            listen = %self.local_addr,
            routes = self.routes,
            max_conns = self.max_conns,
            "relay listening"
        );
        let endpoint = self.endpoint;
        let registry: TunnelRegistry<quinn::Connection> = TunnelRegistry::new();
        let slots = Arc::new(Semaphore::new(self.max_conns));
        let max_handshakes = self.max_conns.max(1).saturating_mul(HANDSHAKES_PER_SLOT);
        let handshakes = Arc::new(Semaphore::new(max_handshakes));
        let sources = Arc::new(SourceShares::new(self.handshakes_per_source));
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                incoming = endpoint.accept() => {
                    let Some(incoming) = incoming else { break };
                    let Some(admitted) =
                        admit_handshake(incoming, &handshakes, max_handshakes, &sources)
                    else {
                        continue;
                    };
                    tokio::spawn(handle_connection(
                        admitted,
                        Arc::clone(&self.tunnel_config),
                        registry.clone(),
                        Arc::clone(&self.tokens),
                        self.preamble_deadline,
                        Arc::clone(&slots),
                    ));
                }
            }
        }
        endpoint.close(0u32.into(), b"shutdown");
        let _ = tokio::time::timeout(SHUTDOWN_DRAIN, endpoint.wait_idle()).await;
        Ok(())
    }
}

/// The in-flight admissions of each source address, by whether its address
/// was validated; see [`DEFAULT_HANDSHAKES_PER_SOURCE`].
#[derive(Debug)]
struct SourceShares {
    share: usize,
    in_flight: Mutex<HashMap<(IpAddr, bool), usize>>,
}

impl SourceShares {
    fn new(share: usize) -> Self {
        Self {
            share: share.max(1),
            in_flight: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(IpAddr, bool), usize>> {
        self.in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Take one of `ip`'s `validated` share, if any is left.
    fn try_take(self: &Arc<Self>, ip: IpAddr, validated: bool) -> Option<SourceShare> {
        let key = (ip, validated);
        let mut in_flight = self.lock();
        let count = in_flight.entry(key).or_insert(0);
        let admitted = *count < self.share;
        if admitted {
            *count += 1;
        }
        drop(in_flight);
        admitted.then(|| SourceShare {
            shares: Arc::clone(self),
            key,
        })
    }
}

/// One held unit of a source's share; returned on drop, and the map entry
/// goes with the last one so it stays bounded by what is in flight.
#[derive(Debug)]
struct SourceShare {
    shares: Arc<SourceShares>,
    key: (IpAddr, bool),
}

impl Drop for SourceShare {
    fn drop(&mut self) {
        let mut in_flight = self.shares.lock();
        if let Some(count) = in_flight.get_mut(&self.key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                in_flight.remove(&self.key);
            }
        }
    }
}

/// A connection admitted to the handshake: its handshake slot, released
/// once the handshake ends, and its source share, released once it is
/// admitted as a tunnel or consumer.
struct AdmittedHandshake {
    incoming: quinn::Incoming,
    handshake: OwnedSemaphorePermit,
    source: SourceShare,
}

/// Take a handshake slot and a source share for `incoming`, or turn it away
/// without spawning anything: a stateless refusal when every slot is busy or
/// the source's validated share is spent, and a Retry (proof the source
/// address is real) once half the slots are busy or the source's unvalidated
/// share is spent, so spoofed Initials can never occupy more than half, nor
/// crowd out the address they spoof.
fn admit_handshake(
    incoming: quinn::Incoming,
    handshakes: &Arc<Semaphore>,
    max_handshakes: usize,
    sources: &Arc<SourceShares>,
) -> Option<AdmittedHandshake> {
    let validated = incoming.remote_address_validated();
    let loaded = handshakes.available_permits() <= max_handshakes / 2;
    if loaded && !validated && incoming.may_retry() {
        let _ = incoming.retry();
        return None;
    }
    let remote = incoming.remote_address();
    let Some(source) = sources.try_take(remote.ip(), validated) else {
        if !validated && incoming.may_retry() {
            let _ = incoming.retry();
        } else {
            tracing::debug!(%remote, "refused: source at its handshake share");
            incoming.refuse();
        }
        return None;
    };
    let Ok(handshake) = Arc::clone(handshakes).try_acquire_owned() else {
        tracing::debug!(%remote, "refused: handshakes at capacity");
        incoming.refuse();
        return None;
    };
    Some(AdmittedHandshake {
        incoming,
        handshake,
        source,
    })
}

/// Drive one accepted connection to its leg by negotiated ALPN, never by
/// what it sends (ADR-0051 invariant 7). The handshake runs under its own
/// slot and deadline; the connection slot is taken only once it completes
/// and is held throughout, and the source share until the leg's admission
/// step (preamble or first stream) ends. The flow-control config is chosen
/// first from the connection-ID tag.
async fn handle_connection(
    admitted: AdmittedHandshake,
    tunnel_config: Arc<quinn::ServerConfig>,
    registry: TunnelRegistry<quinn::Connection>,
    tokens: Arc<CachedRouteTokens>,
    preamble_deadline: Duration,
    slots: Arc<Semaphore>,
) {
    let AdmittedHandshake {
        incoming,
        handshake: handshake_permit,
        source,
    } = admitted;
    let tagged = is_tunnel_cid(&incoming.orig_dst_cid());
    let connecting = if tagged {
        incoming.accept_with(tunnel_config)
    } else {
        incoming.accept()
    };
    let handshake = match connecting {
        Ok(connecting) => tokio::time::timeout(HANDSHAKE_DEADLINE, connecting)
            .await
            .unwrap_or(Err(quinn::ConnectionError::TimedOut)),
        Err(err) => Err(err),
    };
    drop(handshake_permit);
    let conn = match handshake {
        Ok(conn) => conn,
        Err(err) => {
            // Includes consumers the SniGate refused during the handshake.
            tracing::debug!(%err, "handshake failed");
            return;
        }
    };
    // No free slot: an application close needs the finished handshake.
    let Ok(_permit) = slots.try_acquire_owned() else {
        tracing::warn!(remote = %conn.remote_address(), "refused: relay at connection cap");
        conn.close(OVER_CAP_CODE.into(), b"relay at connection capacity");
        return;
    };
    let Some((alpn, server_name)) = handshake_identity(&conn) else {
        conn.close(PROTOCOL_VIOLATION_CODE.into(), b"unreadable handshake data");
        return;
    };
    if alpn == QUIC_RELAY_ALPN {
        let leg = TunnelLeg {
            bounded: tagged,
            tokens: &tokens,
            preamble_deadline,
            source,
        };
        admit_tunnel(conn, leg, &registry).await;
    } else if alpn == QUIC_ALPN {
        bridge_consumer(conn, server_name, source, &registry).await;
    } else {
        // rustls only negotiates advertised protocols; defensive.
        conn.close(PROTOCOL_VIOLATION_CODE.into(), b"unknown protocol");
    }
}

/// The negotiated ALPN and the SNI server name, read back from quinn's
/// rustls handshake data.
fn handshake_identity(conn: &quinn::Connection) -> Option<(Vec<u8>, Option<String>)> {
    let data = conn
        .handshake_data()?
        .downcast::<quinn::crypto::rustls::HandshakeData>()
        .ok()?;
    Some((data.protocol?, data.server_name))
}

/// What admitting one connector tunnel needs beyond the connection.
struct TunnelLeg<'a> {
    /// Whether the connection ID carried the tunnel tag.
    bounded: bool,
    tokens: &'a CachedRouteTokens,
    preamble_deadline: Duration,
    /// Held until the preamble is read (or its deadline passes).
    source: SourceShare,
}

/// Admit (or refuse) a connector tunnel: read the stream-0 auth preamble,
/// resolve its route, claim it, then watch stream 0 until the connection
/// ends.
async fn admit_tunnel(
    conn: quinn::Connection,
    leg: TunnelLeg<'_>,
    registry: &TunnelRegistry<quinn::Connection>,
) {
    let TunnelLeg {
        bounded,
        tokens,
        preamble_deadline,
        source,
    } = leg;
    let remote = conn.remote_address();
    // `accept_bi` resolves on the first bytes, so one deadline covers both.
    let opened = tokio::time::timeout(preamble_deadline, async {
        let (send0, mut recv0) = conn.accept_bi().await.ok()?;
        let token = read_preamble(&mut recv0).await?;
        Some((send0, recv0, token))
    })
    .await
    .ok()
    .flatten();
    drop(source);
    let Some((send0, mut recv0, token)) = opened else {
        tracing::warn!(%remote, "refused: no tunnel auth preamble within deadline");
        conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
        return;
    };

    // As the file stands now; an unreadable store fails closed.
    let store = tokens.current();
    let Some(route) = store.lookup(&token).map(str::to_owned) else {
        tracing::warn!(%remote, "refused: bad tunnel token");
        conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
        return;
    };

    let epoch = registry.claim(&route, conn.clone());
    tracing::info!(route = %route, %remote, bounded, "tunnel up");
    if !bounded {
        tracing::warn!(
            route = %route,
            %remote,
            "tunnel dialed without the connection-ID tag (a connector that predates it): \
             output toward its consumers is not bounded at the relay"
        );
    }

    // Stream-0 watchdog: stream 0 carries only the preamble, so any further
    // byte closes the tunnel. `send0` is held so the stream is not FIN'd.
    let _reserved_send0 = send0;
    let mut byte = [0u8; 1];
    let watchdog = async {
        if let Ok(Some(_)) = recv0.read(&mut byte).await {
            true
        } else {
            // FIN or reset is not a violation: park until the connection ends.
            std::future::pending::<()>().await;
            false
        }
    };
    tokio::select! {
        violation = watchdog => {
            if violation {
                tracing::warn!(route = %route, "stream-0 violation: byte after preamble; closing tunnel");
                conn.close(
                    PROTOCOL_VIOLATION_CODE.into(),
                    b"stream 0 is reserved after the auth preamble",
                );
            }
        }
        _ = conn.closed() => {}
    }
    registry.remove_if_current(&route, epoch);
    tracing::info!(route = %route, %remote, "tunnel down");
}

/// Bridge one consumer's first stream onto a fresh stream over its route's
/// live tunnel. The consumer's bearer preamble crosses opaquely (ADR-0051
/// Decision 4).
async fn bridge_consumer(
    conn: quinn::Connection,
    server_name: Option<String>,
    source: SourceShare,
    registry: &TunnelRegistry<quinn::Connection>,
) {
    let remote = conn.remote_address();
    // Defensive: the SniGate already refused absent SNI at TLS.
    let Some(route) = server_name else {
        tracing::warn!(%remote, "consumer without SNI past the TLS gate; refusing");
        conn.close(ROUTE_OFFLINE_CODE.into(), b"no route requested");
        return;
    };
    let Some(tunnel) = registry.get(&route) else {
        tracing::warn!(%remote, route = %route, "refused: no live tunnel for enrolled route");
        conn.close(ROUTE_OFFLINE_CODE.into(), b"route offline");
        return;
    };
    // Bounded so a handshake-only consumer cannot pin its cap permit.
    let consumer_streams = tokio::time::timeout(CONSUMER_STREAM_DEADLINE, conn.accept_bi()).await;
    drop(source);
    let consumer_streams = match consumer_streams {
        Ok(Ok(streams)) => streams,
        Ok(Err(_)) => return,
        Err(_) => {
            tracing::warn!(%remote, route = %route, "refused: no consumer stream within deadline");
            conn.close(
                PROTOCOL_VIOLATION_CODE.into(),
                b"no consumer stream within deadline",
            );
            return;
        }
    };
    let Ok(Ok((tun_send, tun_recv))) =
        tokio::time::timeout(CONSUMER_STREAM_DEADLINE, tunnel.open_bi()).await
    else {
        tracing::warn!(%remote, route = %route, "tunnel dropped while bridging; refusing consumer");
        conn.close(ROUTE_OFFLINE_CODE.into(), b"route offline");
        return;
    };
    tracing::info!(route = %route, %remote, "consumer bridged");
    let window = SendWindow::new(conn.clone());
    let control_bridge = splice(
        consumer_streams.1,
        tun_send,
        tun_recv,
        TrackedSend::new(consumer_streams.0, window),
    );
    tokio::pin!(control_bridge);

    // The tunnel wire has no consumer-group envelope: a forwarded second
    // stream would look like a new authenticated consumer to the connector.
    // Relay routes stay single-stream, so extra streams are refused.
    let mut refused_streams = 0_u64;
    loop {
        tokio::select! {
            () = &mut control_bridge => break,
            opened = conn.accept_bi() => {
                let Ok((mut cons_send, mut cons_recv)) = opened else { break };
                refused_streams = refused_streams.saturating_add(1);
                let _ = cons_send.reset(PROTOCOL_VIOLATION_CODE.into());
                let _ = cons_recv.stop(PROTOCOL_VIOLATION_CODE.into());
                tracing::debug!(%remote, route = %route, refused_streams, "refused extra stream on single-stream relay route");
            }
            _ = conn.closed() => break,
        }
    }
    tracing::debug!(route = %route, %remote, refused_streams, "consumer bridge ended");
}

/// Read one length-prefixed auth preamble (`len: u32 BE` + raw token),
/// bounded by [`MAX_TOKEN_PREAMBLE`]; `None` on a short read or oversize.
async fn read_preamble(recv: &mut quinn::RecvStream) -> Option<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.ok()?;
    let len = usize::try_from(u32::from_be_bytes(len_buf)).ok()?;
    if len > MAX_TOKEN_PREAMBLE {
        return None;
    }
    let mut token = vec![0u8; len];
    recv.read_exact(&mut token).await.ok()?;
    Some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_async_fails_fast_on_a_malformed_token_store() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = dir.path().join("relay-tokens");
        std::fs::write(&tokens, "broken\n").unwrap();
        let config = RelayConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            cert_path: dir.path().join("relay-cert.pem"),
            key_path: dir.path().join("relay-key.pem"),
            tokens_path: tokens,
            max_conns: DEFAULT_MAX_CONNS,
        };
        let result = RelayRuntime::new(config)
            .run_async(std::future::ready(()))
            .await;
        assert!(matches!(
            result,
            Err(RelayError::MalformedTokenLine { line: 1 })
        ));
    }

    #[tokio::test]
    async fn run_async_starts_provisions_and_shuts_down_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("relay-cert.pem");
        let key = dir.path().join("relay-key.pem");
        let config = RelayConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            cert_path: cert.clone(),
            key_path: key.clone(),
            tokens_path: dir.path().join("relay-tokens"),
            max_conns: DEFAULT_MAX_CONNS,
        };
        // Immediate shutdown: the endpoint binds, then drains and returns.
        RelayRuntime::new(config)
            .run_async(std::future::ready(()))
            .await
            .unwrap();
        assert!(cert.exists() && key.exists(), "certs provisioned on start");
    }
}
