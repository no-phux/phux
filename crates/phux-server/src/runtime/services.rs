//! Start server-owned background work and fold it into graceful shutdown.

use crate::runtime::input_lane;
use crate::runtime::listeners::remote::connector_inner_tls;
use crate::state::SharedState;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

/// Floor on the idle watchdog's re-check interval, so an already-expired
/// budget never spins the runtime with zero-length sleeps.
const IDLE_WATCH_MIN_INTERVAL: Duration = Duration::from_millis(50);

/// Cancel the root token once no client connection has been open for
/// `idle_limit` (ADR-0063). "Unattended" means zero open connections, not
/// zero attached clients, so a harness driving only one-shot verbs is not
/// reaped mid-script. Cancelling the root token takes the same graceful
/// teardown path as Ctrl-C.
pub(in crate::runtime) fn spawn_idle_exit_watchdog(
    state: SharedState,
    idle_limit: Duration,
    root_token: CancellationToken,
) {
    tokio::task::spawn_local(async move {
        loop {
            // While a client is connected `idle_since` is `None`; re-check
            // after a full interval.
            let remaining = state.with(|s| {
                s.idle_since().map_or(idle_limit, |since| {
                    idle_limit.saturating_sub(since.elapsed())
                })
            });
            let nap = remaining.max(IDLE_WATCH_MIN_INTERVAL);
            tokio::select! {
                () = root_token.cancelled() => return,
                () = tokio::time::sleep(nap) => {}
            }
            // Re-read: a client may have come and gone while we slept.
            let expired = state.with(|s| {
                s.idle_since()
                    .is_some_and(|since| since.elapsed() >= idle_limit)
            });
            if expired {
                if !root_token.is_cancelled() {
                    info!(
                        idle_limit_secs = idle_limit.as_secs_f64(),
                        "unattended for the configured idle limit; server exiting"
                    );
                    state.with_mut(crate::state::ServerState::flush_reports_before_shutdown);
                    root_token.cancel();
                }
                return;
            }
        }
    });
}

/// Fold the external shutdown future into the root token. `spawn_local` (not
/// `tokio::spawn`) because the runtime is current-thread with no worker pool.
pub(in crate::runtime) fn spawn_shutdown_folder<F>(
    shutdown: F,
    state: &SharedState,
    root_token: &CancellationToken,
) where
    F: Future<Output = ()> + 'static,
{
    let token = root_token.clone();
    let state = state.clone();
    tokio::task::spawn_local(async move {
        shutdown.await;
        debug!("shutdown future resolved; cancelling root token");
        // Records of what the teardown ends stay for the next start to
        // reconcile, as after a crash.
        state.with_mut(crate::state::ServerState::flush_reports_before_shutdown);
        token.cancel();
    });
}

/// Arm the idle-exit watchdog (ADR-0063) before the seed/resume paths, so a
/// server nobody ever dials still exits.
pub(in crate::runtime) fn arm_idle_exit(
    state: &SharedState,
    exit_after_idle: Option<Duration>,
    root_token: &CancellationToken,
) {
    let Some(idle_limit) = exit_after_idle else {
        return;
    };
    info!(
        idle_limit_secs = idle_limit.as_secs_f64(),
        "ephemeral server: will exit when unattended for the idle limit"
    );
    spawn_idle_exit_watchdog(state.clone(), idle_limit, root_token.clone());
}

/// Retry bounded lifecycle evidence independently of clients and live engines.
pub(in crate::runtime) fn spawn_activation_report_retry(
    state: &SharedState,
    root_token: &CancellationToken,
) {
    let state = state.clone();
    let token = root_token.clone();
    tokio::task::spawn_local(async move {
        loop {
            tokio::select! {
                () = token.cancelled() => return,
                () = tokio::time::sleep(Duration::from_millis(100)) => {
                    state.with_mut(crate::state::ServerState::retry_activation_reports);
                }
            }
        }
    });
}

/// Spawn the hook dispatcher before the seed/resume paths, so a pre-seeded
/// pane fires `after-new-pane` too. No hooks, no dispatcher.
pub(in crate::runtime) fn install_hook_dispatcher(
    state: &SharedState,
    catalog: crate::hooks::HookCatalog,
    socket_path: PathBuf,
) {
    if catalog.is_empty() {
        return;
    }
    let dispatcher = crate::hooks::spawn_hook_dispatcher(
        catalog,
        Some(socket_path),
        Some(phux_plugin::run_log::default_path()),
    );
    state.with_mut(|s| s.set_hook_dispatcher(dispatcher));
}

/// Spawn one link supervisor per validated satellite (ADR-0038) and mirror
/// the relay registry and supervisors into shared state, where a registry
/// reload finds them.
pub(in crate::runtime) fn spawn_hub_links(
    state: &SharedState,
    hub_table: Option<&crate::hub::HubTable>,
    source: Option<crate::hub::SatelliteSource>,
    root_token: &CancellationToken,
) {
    let Some(table) = hub_table else {
        return;
    };
    let relays = crate::hub::relay::HubRelays::default();
    let ssh_program = state.with(|s| s.server_env().ssh_program());
    let adopter = state.with(crate::state::ServerState::satellite_adopter);
    let mut links =
        crate::hub::link::HubLinks::new(relays.clone(), root_token, ssh_program, adopter);
    for (host, entry) in table.iter() {
        links.start(host, entry, state);
    }
    state.with_mut(|s| {
        s.set_hub_relays(relays);
        s.set_hub_links(links, source);
    });
}

/// Supervise the planned outbound connectors. Nothing to supervise without a
/// consumer-token store, which only exists when connectors were configured.
pub(in crate::runtime) fn spawn_connector_supervisors(
    specs: Vec<crate::connector::ConnectorSpec>,
    consumer_tokens: Option<&std::sync::Arc<crate::auth::ReloadingTokenStore>>,
    state: &SharedState,
    input_lane: &input_lane::InputLaneHandle,
    root_token: &CancellationToken,
) {
    let Some(tokens) = consumer_tokens else {
        return;
    };
    let paired = state.with(crate::state::ServerState::workload_mtls_required);
    let env = state.with(crate::state::ServerState::server_env);
    let Some(inner) = connector_inner_tls(paired, &env) else {
        // Fail closed in every posture: a bridged consumer is admitted only
        // inside its end-to-end session, so the relay never reads it
        // (ADR-0180).
        // Named per connector, beside the cause logged above, so an embedder
        // that used to ride the plain relayed stream sees exactly which
        // routes went dark and why (ADR-0180 Decision 6).
        for spec in &specs {
            error!(
                connector = %spec,
                "relay connector disabled: the end-to-end TLS session every relayed consumer \
                 now runs could not be built; fix the cause logged above (an embedder \
                 supplies the server certificate and key as ServerEnv tls_cert/tls_key)"
            );
        }
        return;
    };
    crate::connector::spawn_connectors(specs, tokens, &inner, state, input_lane, root_token);
}
