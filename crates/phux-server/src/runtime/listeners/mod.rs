//! Assemble configured and automatically detected server accept loops.

use crate::runtime::config::ServerError;
use crate::runtime::connections::accept_loop;
use crate::runtime::input_lane;
use crate::runtime::listeners::remote::{build_quic_listener_for, build_ws_listener};
use crate::state::SharedState;
use std::future::Future;
use std::net::SocketAddr;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

pub(super) mod remote;

/// A boxed accept loop, so heterogeneous listeners can share one
/// [`futures_util::future::select_all`] set.
pub(in crate::runtime) type AcceptLoopFuture<'a> =
    std::pin::Pin<Box<dyn Future<Output = Result<(), ServerError>> + 'a>>;

/// The explicitly configured additive listeners, alongside the addresses they
/// were asked for: the auto-bound overlay listener may claim only the ports
/// no explicit address took.
pub(in crate::runtime) struct ConfiguredListeners {
    pub(in crate::runtime) ws_addr: Option<SocketAddr>,
    pub(in crate::runtime) quic_addr: Option<SocketAddr>,
    pub(in crate::runtime) remote: RemoteListeners,
}

impl ConfiguredListeners {
    /// Bind the resolved opt-in addresses that were asked for.
    pub(in crate::runtime) async fn bind(
        ws_addr: Option<SocketAddr>,
        quic_addr: Option<SocketAddr>,
        state: &SharedState,
    ) -> Self {
        Self {
            ws_addr,
            quic_addr,
            remote: RemoteListeners::bind(ws_addr, quic_addr, state).await,
        }
    }

    /// The overlay ports no explicitly configured address claimed.
    pub(in crate::runtime) const fn unclaimed_overlay_ports(&self) -> AutoOverlayPorts {
        AutoOverlayPorts {
            ws: self.ws_addr.is_none(),
            quic: self.quic_addr.is_none(),
        }
    }

    /// The always-on UDS loop plus the additive remote ones.
    pub(in crate::runtime) fn accept_loops<'a>(
        &'a self,
        uds: &'a crate::transport::UdsListener,
        state: &SharedState,
        root_token: &CancellationToken,
        input_lane: &input_lane::InputLaneHandle,
    ) -> Vec<AcceptLoopFuture<'a>> {
        let mut accepts: Vec<AcceptLoopFuture<'a>> = vec![Box::pin(accept_loop(
            uds,
            state.clone(),
            root_token.clone(),
            Some(input_lane.clone()),
        ))];
        accepts.extend(self.remote.accept_loops(state, root_token, input_lane));
        accepts
    }
}

/// Bound WebSocket and QUIC listeners; each bind outcome is recorded for
/// `GET_STATE`.
pub(in crate::runtime) struct RemoteListeners {
    pub(in crate::runtime) ws: Option<crate::transport::WsListener>,
    pub(in crate::runtime) quic: Option<crate::transport::quic::QuicListener>,
}

impl RemoteListeners {
    pub(in crate::runtime) async fn bind(
        ws_addr: Option<SocketAddr>,
        quic_addr: Option<SocketAddr>,
        state: &SharedState,
    ) -> Self {
        let workload_mtls = state.with(crate::state::ServerState::workload_mtls_required);
        let env = state.with(crate::state::ServerState::server_env);
        let ws = match ws_addr {
            Some(addr) => {
                let (listener, slot) = build_ws_listener(addr, workload_mtls, &env).await;
                state.with_mut(|s| s.record_remote_listener(slot));
                listener
            }
            None => None,
        };
        let quic = quic_addr.and_then(|addr| {
            let (listener, slot) = build_quic_listener_for(addr, workload_mtls, &env);
            state.with_mut(|s| s.record_remote_listener(slot));
            // The configured listener adopts satellites that dial in
            // (ADR-0182); it offers their ALPN only when it verifies
            // workload certificates.
            listener.map(|listener| {
                listener.with_satellites(state.with(crate::state::ServerState::satellite_adopter))
            })
        });
        Self { ws, quic }
    }

    pub(in crate::runtime) fn accept_loops<'a>(
        &'a self,
        state: &SharedState,
        root_token: &CancellationToken,
        input_lane: &input_lane::InputLaneHandle,
    ) -> Vec<AcceptLoopFuture<'a>> {
        let mut accepts: Vec<AcceptLoopFuture<'a>> = Vec::new();
        if let Some(ws) = &self.ws {
            accepts.push(Box::pin(accept_loop(
                ws,
                state.clone(),
                root_token.clone(),
                Some(input_lane.clone()),
            )));
        }
        if let Some(quic) = &self.quic {
            accepts.push(Box::pin(accept_loop(
                quic,
                state.clone(),
                root_token.clone(),
                Some(input_lane.clone()),
            )));
        }
        accepts
    }
}

/// Run every accept loop concurrently. The first fatal result cancels the
/// rest, and every loop is still joined so its clients can flush
/// `SERVER_SHUTDOWN`.
pub(in crate::runtime) async fn drive_accept_loops(
    accepts: Vec<AcceptLoopFuture<'_>>,
    root_token: &CancellationToken,
) -> Result<(), ServerError> {
    let (mut result, _index, remaining) = futures_util::future::select_all(accepts).await;
    if result.is_err() {
        root_token.cancel();
    }
    for tail in futures_util::future::join_all(remaining).await {
        if result.is_ok() && tail.is_err() {
            result = tail;
        }
    }
    result
}

/// Default WebSocket port for the auto-configured overlay listener.
pub const DEFAULT_WS_PORT: u16 = 8787;

/// Default QUIC port for the auto-configured overlay listener.
pub const DEFAULT_QUIC_PORT: u16 = 8788;

/// Environment escape hatch disabling the overlay auto-listen entirely.
pub(super) const DISABLE_AUTO_LISTEN_ENV: &str = "PHUX_NO_AUTO_LISTEN";

/// Whether the auto-bound overlay listener (ADR-0081) may run. It is safe to
/// default on because it binds only a detected overlay address (never
/// `0.0.0.0`), is TLS-only, and its token store admits nobody until
/// `phux pair`. `PHUX_NO_AUTO_LISTEN` disables it, and only the default
/// profile binds: ports are host-global, so a dev profile would race the
/// installed server (ADR-0080).
pub(in crate::runtime) const fn auto_overlay_gate_open(
    disabled: bool,
    default_profile: bool,
) -> bool {
    !disabled && default_profile
}

/// The overlay IP to auto-bind. `detect` shells out to `tailscale`, so it
/// must not run at all when the gate is closed.
pub(in crate::runtime) fn resolve_auto_overlay_ip(
    gate_open: bool,
    detect: impl FnOnce() -> Vec<std::net::IpAddr>,
) -> Option<std::net::IpAddr> {
    if !gate_open {
        return None;
    }
    detect().into_iter().next()
}

/// Which auto-bound overlay ports no explicit address claimed.
#[derive(Debug, Clone, Copy)]
pub(in crate::runtime) struct AutoOverlayPorts {
    /// The WebSocket port ([`DEFAULT_WS_PORT`]) is unclaimed.
    pub(in crate::runtime) ws: bool,
    /// The QUIC port ([`DEFAULT_QUIC_PORT`]) is unclaimed.
    pub(in crate::runtime) quic: bool,
}

impl AutoOverlayPorts {
    /// With neither port free, detection has no consumer and must not run.
    pub(in crate::runtime) const fn any(self) -> bool {
        self.ws || self.quic
    }
}

/// Detect the overlay address and serve the auto-bound listeners (ADR-0081)
/// as a peer of the other accept loops. Detection shells out to `tailscale`
/// (a wedged daemon costs ~2s), so it runs on a blocking thread after the
/// accept loops are live: a slow overlay delays this listener, never the
/// server (`tests/lifecycle/overlay_startup.rs`).
///
/// With nothing to bind it parks on cancellation, so the enclosing
/// `select_all` still reads a first completion as "an accept loop ended".
pub(in crate::runtime) async fn serve_auto_overlay_listeners(
    gate_open: bool,
    ports: AutoOverlayPorts,
    detect: fn() -> Vec<std::net::IpAddr>,
    state: SharedState,
    root_token: CancellationToken,
    input_lane: input_lane::InputLaneHandle,
) -> Result<(), ServerError> {
    let detected = tokio::select! {
        () = root_token.cancelled() => return Ok(()),
        joined = tokio::task::spawn_blocking(move || resolve_auto_overlay_ip(gate_open, detect)) => {
            joined.unwrap_or_else(|err| {
                warn!(error = %err, "overlay detection task failed; no auto-bound remote listener");
                None
            })
        }
    };

    let remote = RemoteListeners::bind(
        detected
            .filter(|_| ports.ws)
            .map(|ip| SocketAddr::new(ip, DEFAULT_WS_PORT)),
        detected
            .filter(|_| ports.quic)
            .map(|ip| SocketAddr::new(ip, DEFAULT_QUIC_PORT)),
        &state,
    )
    .await;
    let accepts = remote.accept_loops(&state, &root_token, &input_lane);
    if accepts.is_empty() {
        debug!("no auto-bound overlay listener; nothing detected or nothing to bind");
        root_token.cancelled().await;
        return Ok(());
    }

    drive_accept_loops(accepts, &root_token).await
}
