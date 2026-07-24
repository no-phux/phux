//! Relay runtime: the QUIC endpoint, its accept loop, tunnel admission,
//! and consumer bridging.
//!
//! Mirrors `ServerRuntime`'s `run` / `run_async` split (ADR-0003/0014
//! conventions): one current-thread tokio runtime, plain `tokio::spawn`
//! per connection (the relay holds no `!Send` state, so no `LocalSet`),
//! and per-connection failures logged — nothing a peer sends ever tears
//! down the endpoint.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use phux_protocol::policy::{QUIC_ALPN, QUIC_RELAY_ALPN};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::registry::TunnelRegistry;
use crate::splice::splice;
use crate::tokens::RouteTokenStore;
use crate::{
    AUTH_FAILED_CODE, OVER_CAP_CODE, PROTOCOL_VIOLATION_CODE, ROUTE_OFFLINE_CODE, RelayError, tls,
};

/// Default connection cap (`--max-conns`): the sole limiting knob.
///
/// Over-cap connections complete their handshake and are
/// application-closed with [`crate::OVER_CAP_CODE`]; existing tunnels and
/// consumers are unaffected.
pub const DEFAULT_MAX_CONNS: usize = 64;

/// QUIC idle timeout, matching the server listener and phux-dial so a
/// quiet tunnel or consumer is not reaped before its keep-alive fires.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep-alive interval, comfortably under [`IDLE_TIMEOUT`]. Tunnels also
/// receive keep-alives from the connector's dialer; this covers the
/// relay's own legs.
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// Auth-preamble size bound, mirroring the server QUIC listener's
/// `MAX_TOKEN_PREAMBLE`.
const MAX_TOKEN_PREAMBLE: usize = 256;

/// How long a connector has to present its stream-0 auth preamble before
/// the connection is refused — bounds a slow-loris on the tunnel leg.
const PREAMBLE_DEADLINE: Duration = Duration::from_secs(5);

/// How long an admitted consumer has to open its bidi stream (and the
/// relay to open the matching tunnel stream) before the connection is
/// closed — the consumer-leg sibling of [`PREAMBLE_DEADLINE`]. Without
/// it, a handshake-only consumer holds its cap permit forever: the
/// relay's own keep-alives reset both idle timers, so QUIC never reaps
/// the connection. A legitimate consumer opens its stream immediately to
/// send its bearer preamble, so the bound only fires on stalled peers.
const CONSUMER_STREAM_DEADLINE: Duration = Duration::from_secs(5);

/// How long shutdown waits for close frames to drain before returning.
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(2);

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
            cert_path: tls::default_relay_cert_path(),
            key_path: tls::default_relay_key_path(),
            tokens_path: crate::tokens::default_relay_tokens_path(),
            max_conns: DEFAULT_MAX_CONNS,
        }
    }
}

/// The relay's run loop, embeddable ([`Self::run_async`]) or owning its
/// own current-thread runtime ([`Self::run`]).
///
/// [`Self::bind`] splits the socket bind from serving so a caller can
/// learn the resolved listen address (port 0 becomes the OS-assigned
/// port) before blocking.
#[derive(Debug)]
pub struct RelayRuntime {
    config: RelayConfig,
}

impl RelayRuntime {
    /// Wrap a config, ready to run.
    #[must_use]
    pub const fn new(config: RelayConfig) -> Self {
        Self { config }
    }

    /// Build a current-thread tokio runtime and block on
    /// [`Self::run_async`] until `shutdown` resolves.
    pub fn run(self, shutdown: impl Future<Output = ()>) -> Result<(), RelayError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(self.run_async(shutdown))
    }

    /// Run the relay until `shutdown` resolves or the endpoint closes.
    ///
    /// Startup is fail-fast: an unreadable/malformed token store, broken
    /// certificate material, or a bind failure returns an error. After
    /// startup, per-connection failures are logged and never propagate.
    /// The token store is then re-read per connection attempt, so `phux
    /// relay pair` and line deletion take effect without a restart.
    pub async fn run_async(self, shutdown: impl Future<Output = ()>) -> Result<(), RelayError> {
        self.bind()?.serve(shutdown).await
    }

    /// Validate the state files, build the endpoint, and bind the socket
    /// — everything fail-fast — without serving yet.
    ///
    /// Must be called within a tokio runtime context (quinn attaches its
    /// I/O driver to the current runtime); [`Self::run`] provides one.
    /// The returned [`BoundRelay`] reports the resolved listen address
    /// and serves via [`BoundRelay::serve`].
    pub fn bind(self) -> Result<BoundRelay, RelayError> {
        let config = self.config;
        // Fail-fast validation load; per-connection lookups re-read.
        let store = RouteTokenStore::load(&config.tokens_path)?;
        tls::ensure_self_signed(&config.cert_path, &config.key_path)?;
        let tls_config =
            tls::server_config(&config.cert_path, &config.key_path, &config.tokens_path)?;
        let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls_config)
            .map_err(|err| RelayError::Rustls(rustls::Error::General(err.to_string())))?;
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        let mut transport = quinn::TransportConfig::default();
        if let Ok(idle) = quinn::IdleTimeout::try_from(IDLE_TIMEOUT) {
            transport.max_idle_timeout(Some(idle));
        }
        transport.keep_alive_interval(Some(KEEP_ALIVE));
        server_config.transport_config(Arc::new(transport));

        let endpoint = quinn::Endpoint::server(server_config, config.listen)?;
        // The RESOLVED address: `--listen 127.0.0.1:0` reports the
        // OS-assigned port, not the literal 0.
        let local_addr = endpoint.local_addr()?;
        Ok(BoundRelay {
            endpoint,
            local_addr,
            routes: store.len(),
            tokens_path: config.tokens_path,
            max_conns: config.max_conns,
        })
    }
}

/// A relay whose endpoint is bound but not yet serving — the output of
/// [`RelayRuntime::bind`], consumed by [`Self::serve`].
#[derive(Debug)]
pub struct BoundRelay {
    endpoint: quinn::Endpoint,
    local_addr: SocketAddr,
    routes: usize,
    tokens_path: PathBuf,
    max_conns: usize,
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
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                incoming = endpoint.accept() => {
                    let Some(incoming) = incoming else { break };
                    // Cap enforcement in the accept loop: no free slot means
                    // the handshake completes and the connection is refused
                    // with OVER_CAP — distinguishable at the client, and
                    // isolated from every existing connection.
                    match Arc::clone(&slots).try_acquire_owned() {
                        Ok(permit) => {
                            tokio::spawn(handle_connection(
                                incoming,
                                registry.clone(),
                                self.tokens_path.clone(),
                                permit,
                            ));
                        }
                        Err(_) => {
                            tokio::spawn(refuse_over_cap(incoming));
                        }
                    }
                }
            }
        }
        endpoint.close(0u32.into(), b"shutdown");
        let _ = tokio::time::timeout(SHUTDOWN_DRAIN, endpoint.wait_idle()).await;
        Ok(())
    }
}

/// Complete the handshake for an over-cap connection and refuse it with
/// [`OVER_CAP_CODE`] (an application close requires a finished handshake).
async fn refuse_over_cap(incoming: quinn::Incoming) {
    let Ok(conn) = incoming.await else { return };
    tracing::warn!(remote = %conn.remote_address(), "refused: relay at connection cap");
    conn.close(OVER_CAP_CODE.into(), b"relay at connection capacity");
}

/// Drive one accepted connection to its leg by negotiated ALPN — never by
/// what it sends (ADR-0051 invariant 7). Holds its cap permit for the
/// connection's lifetime.
async fn handle_connection(
    incoming: quinn::Incoming,
    registry: TunnelRegistry<quinn::Connection>,
    tokens_path: PathBuf,
    permit: OwnedSemaphorePermit,
) {
    let _permit = permit;
    let conn = match incoming.await {
        Ok(conn) => conn,
        Err(err) => {
            // Unknown/absent-SNI consumers land here too: the SniGate
            // refused them during the handshake, before any phux byte.
            tracing::debug!(%err, "handshake failed");
            return;
        }
    };
    let Some((alpn, server_name)) = handshake_identity(&conn) else {
        conn.close(PROTOCOL_VIOLATION_CODE.into(), b"unreadable handshake data");
        return;
    };
    if alpn == QUIC_RELAY_ALPN {
        admit_tunnel(conn, &tokens_path, &registry).await;
    } else if alpn == QUIC_ALPN {
        bridge_consumer(conn, server_name, &registry).await;
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

/// Admit (or refuse) a connector tunnel: read the stream-0 auth preamble,
/// resolve it to its enrolled route, claim the route, then park as the
/// stream-0 watchdog until the connection ends.
async fn admit_tunnel(
    conn: quinn::Connection,
    tokens_path: &Path,
    registry: &TunnelRegistry<quinn::Connection>,
) {
    let remote = conn.remote_address();
    // The preamble doubles as the stream-open signal: `accept_bi` does not
    // resolve until the connector's first bytes arrive, so one deadline
    // covers both.
    let opened = tokio::time::timeout(PREAMBLE_DEADLINE, async {
        let (send0, mut recv0) = conn.accept_bi().await.ok()?;
        let token = read_preamble(&mut recv0).await?;
        Some((send0, recv0, token))
    })
    .await
    .ok()
    .flatten();
    let Some((send0, mut recv0, token)) = opened else {
        tracing::warn!(%remote, "refused: no tunnel auth preamble within deadline");
        conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
        return;
    };

    // Re-read per connection attempt (no reload machinery): `phux relay
    // pair` is live immediately; a deleted line refuses the next redial.
    // An unreadable store fails closed.
    let store = match RouteTokenStore::load(tokens_path) {
        Ok(store) => store,
        Err(err) => {
            tracing::warn!(%err, "route-token store unreadable; failing closed");
            RouteTokenStore::default()
        }
    };
    let Some(route) = store.lookup(&token).map(str::to_owned) else {
        tracing::warn!(%remote, "refused: bad tunnel token");
        conn.close(AUTH_FAILED_CODE.into(), b"unauthorized");
        return;
    };

    let epoch = registry.claim(&route, conn.clone());
    tracing::info!(route = %route, %remote, "tunnel up");

    // Stream-0 watchdog. `send0` is held, not dropped: dropping it would
    // FIN the reserved stream. Stream 0 carries ONLY the preamble; any
    // further byte is a protocol violation and closes the tunnel.
    let _reserved_send0 = send0;
    let mut byte = [0u8; 1];
    let watchdog = async {
        if let Ok(Some(_)) = recv0.read(&mut byte).await {
            true
        } else {
            // FIN or reset on stream 0: not extra bytes; park until the
            // connection itself ends.
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

/// Bridge one consumer connection onto its route's live tunnel: exactly
/// one consumer-initiated bidi stream, spliced onto one fresh
/// relay-initiated bidi stream. The consumer's own bearer preamble crosses
/// opaquely — the relay never reads it (ADR-0051 Decision 4).
async fn bridge_consumer(
    conn: quinn::Connection,
    server_name: Option<String>,
    registry: &TunnelRegistry<quinn::Connection>,
) {
    let remote = conn.remote_address();
    // The SniGate already refused unknown/absent SNI at the TLS layer;
    // this re-check is defensive only.
    let Some(route) = server_name else {
        tracing::warn!(%remote, "consumer without SNI past the TLS gate; refusing");
        conn.close(ROUTE_OFFLINE_CODE.into(), b"no route requested");
        return;
    };
    let Some(tunnel) = registry.get(&route) else {
        // Enrolled route, no live tunnel: handshake completed, so this is
        // distinguishable from an unknown route (which never got this far).
        tracing::warn!(%remote, route = %route, "refused: no live tunnel for enrolled route");
        conn.close(ROUTE_OFFLINE_CODE.into(), b"route offline");
        return;
    };
    // `accept_bi` resolves on the consumer's first bytes (its bearer
    // preamble); the whole path up to the splice is bounded by
    // CONSUMER_STREAM_DEADLINE so a handshake-only consumer cannot pin
    // its cap permit forever (the permit is released when this returns).
    // Errors on `accept_bi` just mean the consumer went away.
    let opened = tokio::time::timeout(CONSUMER_STREAM_DEADLINE, async {
        let consumer_streams = conn.accept_bi().await.ok()?;
        Some((consumer_streams, tunnel.open_bi().await))
    })
    .await;
    let ((cons_send, cons_recv), tun_streams) = match opened {
        Ok(Some(streams)) => streams,
        Ok(None) => return,
        Err(_) => {
            tracing::warn!(%remote, route = %route, "refused: no consumer stream within deadline");
            conn.close(
                PROTOCOL_VIOLATION_CODE.into(),
                b"no consumer stream within deadline",
            );
            return;
        }
    };
    let Ok((tun_send, tun_recv)) = tun_streams else {
        tracing::warn!(%remote, route = %route, "tunnel dropped while bridging; refusing consumer");
        conn.close(ROUTE_OFFLINE_CODE.into(), b"route offline");
        return;
    };
    tracing::info!(route = %route, %remote, "consumer bridged");
    splice(cons_recv, tun_send, tun_recv, cons_send).await;
    tracing::debug!(route = %route, %remote, "consumer bridge ended");
}

/// Read one length-prefixed auth preamble (`len: u32 BE` + raw token),
/// bounded by [`MAX_TOKEN_PREAMBLE`]. `None` on any short read or an
/// oversized length — the caller refuses the connection.
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

    #[test]
    fn config_defaults_to_the_cap_and_state_dir_paths() {
        let config = RelayConfig::new("127.0.0.1:4433".parse().unwrap());
        assert_eq!(config.max_conns, DEFAULT_MAX_CONNS);
        assert_eq!(config.max_conns, 64);
        let state = crate::paths::state_dir();
        assert_eq!(config.cert_path, state.join("relay-cert.pem"));
        assert_eq!(config.key_path, state.join("relay-key.pem"));
        assert_eq!(config.tokens_path, state.join("relay-tokens"));
    }

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
