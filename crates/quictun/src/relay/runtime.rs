//! Relay runtime: the QUIC endpoint, its accept loop, tunnel admission,
//! and consumer bridging. One current-thread runtime, one task per
//! connection; per-connection failures are logged and never tear down the
//! endpoint.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::Protocol;
use crate::relay::ingress::{self, TcpIngress};
use crate::relay::registry::TunnelRegistry;
use crate::relay::shares::{Share, Shares};
use crate::relay::tokens::{CachedRouteTokens, RouteTokenStore};
use crate::relay::{RelayError, tls};
use crate::splice::{Tee, splice};
use crate::tunnel::{
    AUTH_FAILED_CODE, OVER_CAP_CODE, PROTOCOL_VIOLATION_CODE, ROUTE_OFFLINE_CODE, read_preamble,
};
use crate::window::{SendWindow, TrackedSend};

/// Default connection cap (`--max-conns`). Over-cap connections complete
/// their handshake and are closed with [`crate::tunnel::OVER_CAP_CODE`].
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

/// QUIC idle timeout, matching [`crate::connect`]'s.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep-alive interval, well under [`IDLE_TIMEOUT`].
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// How long a connector has to present its stream-0 auth preamble; bounds a
/// slow-loris on the tunnel leg. Tests shorten it via
/// [`RelayRuntime::with_preamble_deadline`].
const PREAMBLE_DEADLINE: Duration = Duration::from_secs(5);

/// How long an admitted consumer has to open its stream (and the relay the
/// matching tunnel stream). Without it a handshake-only consumer would hold
/// its cap permit forever, since keep-alives stop QUIC from reaping it.
pub(crate) const CONSUMER_STREAM_DEADLINE: Duration = Duration::from_secs(5);

/// How long shutdown waits for close frames to drain before returning.
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(2);

/// Per-stream receive window a tunnel grants the service: one stream per
/// bridged consumer.
///
/// Credit the relay has granted but not forwarded is lag. Held small, a slow
/// consumer blocks the service's writer quickly, so an interactive service
/// can drop or coalesce output instead of queueing seconds of it. Only
/// tagged tunnels get it (see [`Protocol::cid_tag`]): consumers keep quinn's
/// defaults so uploads stay fast, and untagged tunnels are logged. Being per
/// stream, one stalled consumer never holds back another.
///
/// This caps each consumer at one window per service-to-relay round trip
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
    /// The ALPNs and connection-ID tag this relay serves.
    pub protocol: Protocol,
    /// Consumers one route may have bridged at once, so one tenant cannot
    /// take every connection slot; `None` leaves only [`Self::max_conns`].
    pub max_conns_per_route: Option<usize>,
    /// A TCP listener for clients that cannot speak QUIC, routed on their
    /// TLS `ClientHello`'s server name; `None` serves QUIC only.
    pub tcp_ingress: Option<TcpIngress>,
}

/// The relay's run loop: [`Self::run_async`], or [`Self::bind`] then
/// [`BoundRelay::serve`] to learn the resolved listen address first.
#[derive(Debug)]
pub struct RelayRuntime {
    config: RelayConfig,
    preamble_deadline: Duration,
    handshakes_per_source: usize,
    /// What tests watch the relay do; empty in production.
    observers: Observers,
}

/// What a test may watch the relay do, without changing what it does. Both
/// are `None` in production.
#[derive(Debug, Clone, Default)]
pub(crate) struct Observers {
    /// Consumer SNIs rustls read from completed handshakes.
    pub(crate) consumer_sni: Option<Arc<Mutex<Vec<String>>>>,
    /// Every byte the relay forwarded for a consumer, both directions: the
    /// relay's whole view of a bridged session.
    pub(crate) consumer_bytes: Option<Arc<Mutex<Vec<u8>>>>,
}

impl RelayRuntime {
    /// Wrap a config, ready to run.
    #[must_use]
    pub const fn new(config: RelayConfig) -> Self {
        Self {
            config,
            preamble_deadline: PREAMBLE_DEADLINE,
            handshakes_per_source: DEFAULT_HANDSHAKES_PER_SOURCE,
            observers: Observers {
                consumer_sni: None,
                consumer_bytes: None,
            },
        }
    }

    /// Record each consumer SNI rustls accepted. Does not change the listen
    /// socket, the SNI gate, or which handshakes are admitted.
    #[must_use]
    pub fn with_observed_consumer_sni(mut self, observed: Arc<Mutex<Vec<String>>>) -> Self {
        self.observers.consumer_sni = Some(observed);
        self
    }

    /// Append every byte the relay forwards between a consumer and its
    /// tunnel, in both directions, as it forwards it. This is everything a
    /// relay operator could record of a bridged session; it does
    /// not change what is forwarded.
    #[must_use]
    pub fn with_observed_consumer_bytes(mut self, observed: Arc<Mutex<Vec<u8>>>) -> Self {
        self.observers.consumer_bytes = Some(observed);
        self
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
    /// The token store is re-read whenever it changed, so minting a route
    /// and deleting a line take effect without a restart.
    pub async fn run_async(self, shutdown: impl Future<Output = ()>) -> Result<(), RelayError> {
        self.bind()?.serve(shutdown).await
    }

    /// Validate the state files, build the endpoint, and bind the socket
    /// without serving yet, so the caller can learn the resolved address.
    /// Must be called within a tokio runtime context.
    pub fn bind(self) -> Result<BoundRelay, RelayError> {
        let config = self.config;
        config.protocol.validate().map_err(RelayError::Protocol)?;
        let preamble_deadline = self.preamble_deadline;
        // Fail-fast validation load; per-connection lookups re-read it
        // whenever the file changes.
        let store = RouteTokenStore::load(&config.tokens_path)?;
        let tokens = Arc::new(CachedRouteTokens::new(config.tokens_path.clone()));
        tls::ensure_self_signed(&config.cert_path, &config.key_path)?;
        let tls_config = tls::server_config(
            &config.cert_path,
            &config.key_path,
            &config.protocol,
            Arc::clone(&tokens),
        )?;
        let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls_config)
            .map_err(|err| RelayError::Rustls(rustls::Error::General(err.to_string())))?;
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        // Same crypto, bounded per-stream window, chosen per connection.
        let mut tunnel_config = server_config.clone();
        server_config.transport_config(Arc::new(transport_config(false)));
        tunnel_config.transport_config(Arc::new(transport_config(true)));

        let endpoint = quinn::Endpoint::server(server_config, config.listen)?;
        let local_addr = endpoint.local_addr()?;
        let tcp = config.tcp_ingress.map(ingress::bind).transpose()?;
        Ok(BoundRelay {
            endpoint,
            tcp,
            tunnel_config: Arc::new(tunnel_config),
            protocol: config.protocol,
            local_addr,
            routes: store.len(),
            tokens,
            max_conns: config.max_conns,
            max_conns_per_route: config.max_conns_per_route,
            preamble_deadline,
            handshakes_per_source: self.handshakes_per_source,
            observers: self.observers,
        })
    }
}

/// What every connection task of one serving relay shares.
#[derive(Debug)]
pub(crate) struct Shared {
    /// The server config a tagged tunnel is accepted with: the endpoint's
    /// crypto plus [`TUNNEL_STREAM_RECEIVE_WINDOW`].
    tunnel_config: Arc<quinn::ServerConfig>,
    pub(crate) protocol: Protocol,
    pub(crate) registry: TunnelRegistry<quinn::Connection>,
    pub(crate) tokens: Arc<CachedRouteTokens>,
    preamble_deadline: Duration,
    /// Connection slots, `max_conns` of them, held for a connection's life.
    pub(crate) slots: Arc<Semaphore>,
    /// Admissions in progress (a QUIC handshake, a TCP `ClientHello`), held
    /// apart from [`Self::slots`] so peers that stall before admission can
    /// never lock out connectors and consumers.
    pub(crate) handshakes: Arc<Semaphore>,
    /// Admissions in progress per source address (and whether it was
    /// validated); see [`DEFAULT_HANDSHAKES_PER_SOURCE`].
    pub(crate) sources: Arc<Shares<(IpAddr, bool)>>,
    /// Bridged consumers per route, when capped.
    route_consumers: Option<Arc<Shares<String>>>,
    pub(crate) observers: Observers,
}

/// A route's consumer unit: `None` for an uncapped route.
pub(crate) type RouteShare = Option<Share<String>>;

impl Shared {
    /// One of `route`'s consumer units, or `None` when the route is at its
    /// cap.
    pub(crate) fn take_route_share(&self, route: &str) -> Option<RouteShare> {
        self.route_consumers.as_ref().map_or(Some(None), |shares| {
            shares.try_take(route.to_owned()).map(Some)
        })
    }

    /// Append bytes the relay forwarded outside a [`Tee`] to the tap.
    pub(crate) fn observe_bytes(&self, bytes: &[u8]) {
        if let Some(sink) = &self.observers.consumer_bytes {
            sink.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(bytes);
        }
    }
}

/// A relay whose endpoint is bound but not yet serving — the output of
/// [`RelayRuntime::bind`], consumed by [`Self::serve`].
#[derive(Debug)]
pub struct BoundRelay {
    endpoint: quinn::Endpoint,
    tcp: Option<ingress::BoundIngress>,
    tunnel_config: Arc<quinn::ServerConfig>,
    protocol: Protocol,
    local_addr: SocketAddr,
    routes: usize,
    tokens: Arc<CachedRouteTokens>,
    max_conns: usize,
    max_conns_per_route: Option<usize>,
    preamble_deadline: Duration,
    handshakes_per_source: usize,
    observers: Observers,
}

impl BoundRelay {
    /// The endpoint's resolved bound address: when the config asked for
    /// port 0, this carries the OS-assigned port.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The TCP ingress listener's resolved address, when one is configured.
    #[must_use]
    pub fn tcp_addr(&self) -> Option<SocketAddr> {
        self.tcp.as_ref().map(ingress::BoundIngress::local_addr)
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
        let max_handshakes = self.max_conns.max(1).saturating_mul(HANDSHAKES_PER_SLOT);
        let handshakes = Arc::new(Semaphore::new(max_handshakes));
        let shared = Arc::new(Shared {
            handshakes: Arc::clone(&handshakes),
            tunnel_config: self.tunnel_config,
            protocol: self.protocol,
            registry: TunnelRegistry::new(),
            tokens: self.tokens,
            preamble_deadline: self.preamble_deadline,
            slots: Arc::new(Semaphore::new(self.max_conns)),
            sources: Arc::new(Shares::new(self.handshakes_per_source)),
            route_consumers: self
                .max_conns_per_route
                .map(|cap| Arc::new(Shares::new(cap))),
            observers: self.observers,
        });
        // TCP bridges are tasks of their own; they end with the relay.
        let mut tcp_bridges = tokio::task::JoinSet::new();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                incoming = endpoint.accept() => {
                    let Some(incoming) = incoming else { break };
                    let Some(admitted) =
                        admit_handshake(incoming, &handshakes, max_handshakes, &shared.sources)
                    else {
                        continue;
                    };
                    tokio::spawn(handle_connection(admitted, Arc::clone(&shared)));
                }
                accepted = ingress::accept(self.tcp.as_ref()) => {
                    if let Some((stream, remote, domain)) = accepted {
                        tcp_bridges.spawn(ingress::bridge(stream, remote, domain, Arc::clone(&shared)));
                    }
                }
                Some(_) = tcp_bridges.join_next(), if !tcp_bridges.is_empty() => {}
            }
        }
        tcp_bridges.abort_all();
        endpoint.close(0u32.into(), b"shutdown");
        let _ = tokio::time::timeout(SHUTDOWN_DRAIN, endpoint.wait_idle()).await;
        Ok(())
    }
}

/// A connection admitted to the handshake: its handshake slot, released
/// once the handshake ends, and its source share, released once it is
/// admitted as a tunnel or consumer.
struct AdmittedHandshake {
    incoming: quinn::Incoming,
    handshake: OwnedSemaphorePermit,
    source: Share<(IpAddr, bool)>,
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
    sources: &Arc<Shares<(IpAddr, bool)>>,
) -> Option<AdmittedHandshake> {
    let validated = incoming.remote_address_validated();
    let loaded = handshakes.available_permits() <= max_handshakes / 2;
    if loaded && !validated && incoming.may_retry() {
        let _ = incoming.retry();
        return None;
    }
    let remote = incoming.remote_address();
    let Some(source) = sources.try_take((remote.ip(), validated)) else {
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
/// what it sends. The handshake runs under its own
/// slot and deadline; the connection slot is taken only once it completes
/// and is held throughout, and the source share until the leg's admission
/// step (preamble or first stream) ends. The flow-control config is chosen
/// first from the connection-ID tag.
async fn handle_connection(admitted: AdmittedHandshake, shared: Arc<Shared>) {
    let AdmittedHandshake {
        incoming,
        handshake: handshake_permit,
        source,
    } = admitted;
    let tagged = shared.protocol.cid_tag.matches(&incoming.orig_dst_cid());
    let connecting = if tagged {
        incoming.accept_with(Arc::clone(&shared.tunnel_config))
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
    let Ok(_permit) = Arc::clone(&shared.slots).try_acquire_owned() else {
        tracing::warn!(remote = %conn.remote_address(), "refused: relay at connection cap");
        conn.close(OVER_CAP_CODE.into(), b"relay at connection capacity");
        return;
    };
    let Some((alpn, server_name)) = handshake_identity(&conn) else {
        conn.close(PROTOCOL_VIOLATION_CODE.into(), b"unreadable handshake data");
        return;
    };
    if alpn == shared.protocol.tunnel_alpn {
        let leg = TunnelLeg {
            bounded: tagged,
            tokens: &shared.tokens,
            preamble_deadline: shared.preamble_deadline,
            source,
        };
        admit_tunnel(conn, leg, &shared.registry).await;
    } else if shared.protocol.is_consumer_alpn(&alpn) {
        note_consumer_sni(
            shared.observers.consumer_sni.as_ref(),
            server_name.as_deref(),
        );
        bridge_consumer(conn, server_name, source, &shared).await;
    } else {
        // rustls only negotiates advertised protocols; defensive.
        conn.close(PROTOCOL_VIOLATION_CODE.into(), b"unknown protocol");
    }
}

/// Append a consumer SNI rustls already accepted. Absent observer or absent
/// name records nothing; the handshake decision has already been made.
pub(crate) fn note_consumer_sni(observed: Option<&Arc<Mutex<Vec<String>>>>, name: Option<&str>) {
    let (Some(observed), Some(name)) = (observed, name) else {
        return;
    };
    observed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(name.to_owned());
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
    source: Share<(IpAddr, bool)>,
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
/// live tunnel. What crosses is the consumer's end-to-end TLS session with
/// the service, ciphertext the relay holds no key for; the tap records it
/// for tests.
async fn bridge_consumer(
    conn: quinn::Connection,
    server_name: Option<String>,
    source: Share<(IpAddr, bool)>,
    shared: &Shared,
) {
    let remote = conn.remote_address();
    // Defensive: the SniGate already refused absent SNI at TLS.
    let Some(route) = server_name else {
        tracing::warn!(%remote, "consumer without SNI past the TLS gate; refusing");
        conn.close(ROUTE_OFFLINE_CODE.into(), b"no route requested");
        return;
    };
    let Some(tunnel) = shared.registry.get(&route) else {
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
    // Taken only once the consumer has opened its stream, so a
    // handshake-only consumer never holds one of the route's units.
    let Some(_route_share) = shared.take_route_share(&route) else {
        tracing::warn!(%remote, route = %route, "refused: route at its consumer cap");
        conn.close(OVER_CAP_CODE.into(), b"route at its consumer capacity");
        return;
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
    let observed = &shared.observers.consumer_bytes;
    let control_bridge = splice(
        Tee::new(consumer_streams.1, observed.clone()),
        tun_send,
        Tee::new(tun_recv, observed.clone()),
        TrackedSend::new(consumer_streams.0, window),
        shared.protocol.half_close,
    );
    tokio::pin!(control_bridge);

    // The tunnel wire has no consumer-group envelope: a forwarded second
    // stream would look like a new authenticated consumer to the connector.
    // Relay routes stay single-stream, so extra streams are refused.
    let mut refused_streams = 0_u64;
    loop {
        tokio::select! {
            () = &mut control_bridge => {
                // Dropping the connection discards stream data quinn has not
                // delivered yet, such as the tail of a reply; the consumer
                // closes once it has read the FIN, so give it that long.
                let _ = tokio::time::timeout(SHUTDOWN_DRAIN, conn.closed()).await;
                break;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config_in(dir: &std::path::Path) -> RelayConfig {
        let protocol = Protocol::new(b"t/1", b"c/1", crate::CidTag(*b"test"));
        RelayConfig::in_dir("127.0.0.1:0".parse().unwrap(), dir, protocol)
    }

    #[tokio::test]
    async fn run_async_fails_fast_on_a_malformed_token_store() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path());
        std::fs::write(&config.tokens_path, "broken\n").unwrap();
        let result = RelayRuntime::new(config)
            .run_async(std::future::ready(()))
            .await;
        assert!(matches!(
            result,
            Err(RelayError::MalformedTokenLine { line: 1 })
        ));
    }

    #[tokio::test]
    async fn bind_refuses_a_protocol_that_cannot_tell_its_legs_apart() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path());
        config.protocol.consumer_alpns.push(b"t/1".to_vec());
        let result = RelayRuntime::new(config).bind();
        assert!(matches!(result, Err(RelayError::Protocol(_))), "{result:?}");
    }

    #[tokio::test]
    async fn run_async_starts_provisions_and_shuts_down_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path());
        let (cert, key) = (config.cert_path.clone(), config.key_path.clone());
        // Immediate shutdown: the endpoint binds, then drains and returns.
        RelayRuntime::new(config)
            .run_async(std::future::ready(()))
            .await
            .unwrap();
        assert!(cert.exists() && key.exists(), "certs provisioned on start");
    }
}
