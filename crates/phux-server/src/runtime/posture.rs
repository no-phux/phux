//! Resolve and install the authority posture before binding remote entry points.

use crate::runtime::config::{ServerConfig, ServerError};
use crate::runtime::listeners::{AutoOverlayPorts, auto_overlay_gate_open};
use crate::runtime::workload_auth;
use crate::state::SharedState;
use phux_protocol::wire::{ListenerDisabledReason, RemoteListenerSlot, RemoteListenerTransport};
use std::net::SocketAddr;
use tracing::{error, info};

/// The engine a posture calls for; `None` keeps the state's current engine.
pub(in crate::runtime) type PostureEngine = Option<std::sync::Arc<dyn crate::policy::PolicyEngine>>;

/// Resolve the startup posture from `[policy] mode` and the configured entry
/// points, refuse the contradictions (`docs/remote.md#who-can-connect`), and
/// build the engine it calls for.
///
/// A `resuming` image (ADR-0032) never exits on a refusal: the previous
/// process is already gone, and exiting would end every pane it handed over.
/// It takes [`crate::policy::PolicyPosture::resume_fallback`] instead, logs
/// the refusal, and returns the fallback so the caller can leave the remote
/// entry points unbound.
pub(in crate::runtime) fn startup_policy(
    cfg: &ServerConfig,
    remote_listener: bool,
    resuming: bool,
) -> Result<
    (
        crate::policy::PolicyPosture,
        PostureEngine,
        Option<crate::policy::ResumeFallback>,
    ),
    ServerError,
> {
    let inputs = crate::policy::PostureInputs {
        mode: cfg.policy_mode,
        retired_workload_mtls_env: cfg.env.retired_workload_mtls,
        remote_listener,
        workload_authority_configured: cfg.env.workload_authority_configured(),
        embedder_engine: cfg.policy_engine.is_some(),
    };
    let (posture, fallback) = match crate::policy::PolicyPosture::resolve(inputs) {
        Ok(posture) => (posture, None),
        Err(refusal) if resuming => {
            let fallback = crate::policy::PolicyPosture::resume_fallback(inputs, refusal);
            error!(
                %refusal,
                posture = ?fallback.posture,
                remote_unbound = fallback.remote_dropped,
                "resumed across an upgrade with a configuration a cold start refuses; \
                 kept every pane and left the remote entry points unbound; fix the \
                 configuration, then `phux upgrade` again to bind them"
            );
            (fallback.posture, Some(fallback))
        }
        Err(refusal) => return Err(refusal.into()),
    };
    let engine = posture_policy_engine(cfg, posture)?;
    Ok((posture, engine, fallback))
}

/// Report each remote listener a resume left unbound for its posture in
/// `GET_STATE`, so `phux doctor` and `phux pair` name the cause.
pub(in crate::runtime) fn record_refused_listeners(
    state: &SharedState,
    refused: &[(RemoteListenerTransport, SocketAddr)],
) {
    state.with_mut(|s| {
        for &(transport, addr) in refused {
            s.record_remote_listener(RemoteListenerSlot::disabled(
                transport,
                Some(addr.to_string()),
                ListenerDisabledReason::PolicyModeRefused,
            ));
        }
    });
}

/// The engine the posture calls for, or `None` when the embedder injected
/// one through [`ServerConfig::policy_engine`].
///
/// `paired` loads the workload authority here, before anything binds, so
/// missing, malformed, or unsafe material refuses to start
/// (`docs/remote.md#who-can-connect`).
pub(in crate::runtime) fn posture_policy_engine(
    cfg: &ServerConfig,
    posture: crate::policy::PolicyPosture,
) -> Result<Option<std::sync::Arc<dyn crate::policy::PolicyEngine>>, ServerError> {
    use crate::policy::{PolicyPosture, ScopedPolicy};

    if cfg.policy_engine.is_some() {
        return Ok(None);
    }
    let engine: std::sync::Arc<dyn crate::policy::PolicyEngine> = match posture {
        PolicyPosture::Local | PolicyPosture::Embedded => {
            std::sync::Arc::new(ScopedPolicy::local())
        }
        PolicyPosture::Paired => {
            let authority = workload_auth::WorkloadAuth::configured(&cfg.env)
                .map_err(ServerError::PolicyMaterial)?;
            std::sync::Arc::new(ScopedPolicy::paired(authority.registry))
        }
    };
    Ok(Some(engine))
}

/// Whether a remote entry point is configured: a WebSocket, QUIC, or
/// WebTransport listener (flag or environment) or a relay connector. With no
/// mode, or `local`, the server refuses to start beside one
/// (`docs/remote.md#who-can-connect`).
pub(in crate::runtime) const fn remote_listener_configured(
    ws_addr: Option<SocketAddr>,
    quic_addr: Option<SocketAddr>,
    webtransport: bool,
    connectors: bool,
) -> bool {
    ws_addr.is_some() || quic_addr.is_some() || webtransport || connectors
}

/// Mirror the posture into shared state: whether TLS listeners require a
/// workload certificate, and the engine that mints each connection's grant.
pub(in crate::runtime) fn install_policy_posture(
    state: &SharedState,
    posture: crate::policy::PolicyPosture,
    engine: Option<std::sync::Arc<dyn crate::policy::PolicyEngine>>,
) {
    state.with_mut(|s| {
        s.set_policy_posture(posture);
        if let Some(engine) = engine {
            s.set_policy_engine(engine);
        }
    });
}

/// Whether the auto-bound overlay listener may run: some overlay port is
/// unclaimed, its gate is open, and the posture admits a remote door, which
/// `[policy] mode = "local"` never does (`docs/remote.md#who-can-connect`).
///
/// With no mode set (`mode_unset`) the server is `local` too; where the
/// overlay listener would otherwise have bound, that is said once at info,
/// since such a host served it with no configuration before ADR-0179.
pub(in crate::runtime) fn auto_overlay_allowed(
    ports: AutoOverlayPorts,
    state: &SharedState,
    mode_unset: bool,
) -> bool {
    let gate_open = ports.any()
        && auto_overlay_gate_open(
            state.with(|s| s.server_env().no_auto_listen),
            phux_config::instance::is_default_profile(),
        );
    let local_only = state
        .with(crate::state::ServerState::policy_posture)
        .is_local();
    if gate_open && local_only && mode_unset {
        info!(
            "overlay listener not bound: no [policy] mode is set, so the server admits its \
             owner socket only; set [policy] mode = \"paired\" in config.toml to serve \
             enrolled clients over an overlay network"
        );
    }
    gate_open && !local_only
}
