//! Assemble startup in order, drive the listeners, and unwind graceful shutdown.

use super::{ListeningHook, ServerRuntime};
use crate::runtime::config::ServerError;
#[cfg(feature = "webtransport")]
use crate::runtime::connections::accept_loop;
use crate::runtime::connections::socket::{socket_identity, unlink_socket_if_ours};
use crate::runtime::listeners::{
    ConfiguredListeners, drive_accept_loops, serve_auto_overlay_listeners,
};
use crate::runtime::posture::{
    auto_overlay_allowed, install_policy_posture, record_refused_listeners,
    remote_listener_configured, startup_policy,
};
use crate::runtime::services::{
    arm_idle_exit, install_hook_dispatcher, spawn_connector_supervisors, spawn_hub_links,
    spawn_shutdown_folder,
};
use crate::runtime::startup::{
    adopt_or_bind_listener, install_hub_table, load_connector_consumer_tokens,
    mirror_config_into_state, open_org_store, read_resume_blob, resume_session_tree,
    seed_initial_session,
};
use crate::runtime::{delegation, input_lane, native_agent, revocation, upload};
use crate::state::SharedState;
use phux_protocol::wire::RemoteListenerTransport;
use std::future::Future;
use tokio::task::LocalSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

#[cfg(feature = "webtransport")]
use crate::runtime::listeners::remote::build_wt_listener;

impl ServerRuntime {
    /// Run the server until `shutdown` resolves, on a [`LocalSet`] driven by
    /// the current runtime (ADR-0014: pane actors are `!Send`).
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: server runs on a LocalSet; per-pane actors are !Send"
    )]
    pub async fn run_async<F>(self, shutdown: F) -> Result<(), ServerError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        // The body is a large state machine. Boxing it keeps callers that
        // await `run_async` (the upgrade tests in particular) under the
        // large-future lint.
        Box::pin(self.run_inner(shutdown)).await
    }

    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: server runs on a LocalSet; per-pane actors are !Send"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "startup binds every transport once; splitting hides the accept-set assembly"
    )]
    async fn run_inner<F>(self, shutdown: F) -> Result<(), ServerError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let socket_path = self.cfg.socket_path.clone();

        let state = SharedState::new();

        let hub_table = install_hub_table(&state, self.hub, &self.satellites)?;

        // Connector plans and the policy posture are startup gates: bad
        // configuration fails before anything is bound.
        let mut connector_specs = crate::connector::plan_connectors(&self.connectors)?;
        let uplink_specs = crate::uplink::plan_uplinks(&self.uplinks)?;
        // Each opt-in address: the flag wins over the environment.
        let env = &self.cfg.env;
        let mut ws_addr = self.ws_addr.or(env.ws_addr);
        let mut quic_addr = self.quic_addr.or(env.quic_addr);
        #[cfg(feature = "webtransport")]
        let mut webtransport_addr = self.wt_addr.or(env.wt_addr);
        #[cfg(feature = "webtransport")]
        let webtransport = webtransport_addr.is_some();
        #[cfg(not(feature = "webtransport"))]
        let webtransport = false;
        let (posture, posture_engine, fallback) = startup_policy(
            &self.cfg,
            remote_listener_configured(
                ws_addr,
                quic_addr,
                webtransport,
                !self.connectors.is_empty(),
            ),
            self.resume_fd.is_some(),
        )?;
        // A resume the posture refuses keeps its panes and binds no remote
        // entry point; the flags stay in `runtime_flags`, so the next upgrade
        // after the operator sets a mode binds them again.
        let mut refused_listeners = Vec::new();
        if fallback.is_some_and(|fallback| fallback.remote_dropped) {
            refused_listeners.extend(
                [
                    (RemoteListenerTransport::Wss, ws_addr.take()),
                    (RemoteListenerTransport::Quic, quic_addr.take()),
                    #[cfg(feature = "webtransport")]
                    (RemoteListenerTransport::Wt, webtransport_addr.take()),
                ]
                .into_iter()
                .filter_map(|(transport, addr)| addr.map(|addr| (transport, addr))),
            );
            if !connector_specs.is_empty() {
                warn!(
                    connectors = connector_specs.len(),
                    "relay connectors not started: the [policy] mode admits no remote consumer"
                );
                connector_specs.clear();
            }
        }
        let connector_consumer_tokens = load_connector_consumer_tokens(&connector_specs, env)?;

        let resume_blob = read_resume_blob(self.resume_fd, &self.inherited_upgrade)?;
        let listener = adopt_or_bind_listener(
            &socket_path,
            resume_blob.as_ref().map(|blob| blob.listener_fd),
        )
        .await?;

        // Remember which inode we bound so shutdown only unlinks our socket.
        let bound_socket = socket_identity(&socket_path);

        let runtime_flags = self.runtime_flags();
        let autosave = self.autosave;
        let on_listening = self.on_listening;
        state.with_mut(|s| {
            s.set_upgrade_context(listener.as_raw_fd(), socket_path.clone(), runtime_flags);
        });

        mirror_config_into_state(&self.cfg, &socket_path, &state);
        install_policy_posture(&state, posture, posture_engine);
        record_refused_listeners(&state, &refused_listeners);
        let mode_unset = self.cfg.policy_mode.is_none();

        let pre_seeded = self.cfg.pre_seeded_session.clone();
        let org_store = self.cfg.org_store.clone();
        let seed_with_pty = self.cfg.seed_with_pty;
        let seed_command = self.cfg.seed_command.clone();
        let scrollback = self.cfg.scrollback;
        let overlay_detect = self.overlay_detect;
        let hook_catalog = self.cfg.hook_catalog.clone();
        let hook_socket_path = socket_path.clone();
        let autosave_socket_path = socket_path.clone();
        let exit_after_idle = self.cfg.exit_after_idle;
        let plugin_startup = self.cfg.plugin_startup.clone();
        let startup_socket = socket_path.clone();
        let satellite_source = self.satellite_source.clone();
        let uplink_source = self.uplink_source.clone();
        let uplink_socket = socket_path.clone();
        // Input routing runs on its own OS thread (ADR-0044) so keystrokes
        // are not queued behind output broadcast; its `Drop` joins it.
        let input_lane = input_lane::spawn_input_lane(state.clone())?;
        let input_lane_handle = input_lane.handle();
        let local = LocalSet::new();
        // Parent of every per-client and per-pane token; `shutdown` is
        // folded into it.
        let root_token = CancellationToken::new();
        let result = local
            .run_until(async move {
                spawn_shutdown_folder(shutdown, &state, &root_token);
                arm_idle_exit(&state, exit_after_idle, &root_token);
                install_hook_dispatcher(&state, hook_catalog, hook_socket_path);
                state.with_mut(|s| {
                    s.arm_hub_reload(satellite_source.clone(), root_token.clone());
                });
                spawn_hub_links(&state, hub_table.as_ref(), satellite_source, &root_token);
                // Satellite uplinks (ADR-0182): hub streams splice to the
                // socket bound above.
                state.with_mut(|s| {
                    let uplinks = s.uplinks_mut();
                    uplinks.arm(uplink_source, uplink_socket, root_token.clone());
                    uplinks.apply(uplink_specs);
                });
                // Live revocation (workload-auth §7).
                revocation::spawn_revocation_watcher(&state, &root_token);
                // Off the runtime thread: a large upload directory is a
                // directory walk.
                let upload_env = state.with(crate::state::ServerState::server_env);
                drop(tokio::task::spawn_blocking(move || {
                    upload::sweep_stale_partials_at_startup(&upload_env);
                }));
                spawn_connector_supervisors(
                    connector_specs,
                    connector_consumer_tokens.as_ref(),
                    &state,
                    &input_lane_handle,
                    &root_token,
                );

                // Configured listeners bind before the session tree exists, and
                // nothing slow may run between "a pane exists" and "the accept
                // loop runs": a live pane must never race an unreachable server.
                let configured = ConfiguredListeners::bind(ws_addr, quic_addr, &state).await;
                #[cfg(feature = "webtransport")]
                let webtransport_listener = webtransport_addr.and_then(|addr| {
                    let env = state.with(crate::state::ServerState::server_env);
                    let paired = state.with(crate::state::ServerState::workload_mtls_required);
                    let (listener, slot) = build_wt_listener(addr, paired, &env);
                    state.with_mut(|s| s.record_remote_listener(slot));
                    listener
                });
                if let ListeningHook(Some(hook)) = on_listening {
                    hook(&state.with(|s| s.remote_listeners().clone()));
                }

                // ADR-0162 startup order: open the store, resume (or import)
                // the upgrade blob, persist a first start, reconcile what a
                // previous run left live, then seed. Nothing relaunches.
                let cold_start = resume_blob.is_none();
                open_org_store(&state, &org_store)?;
                if let Some(blob) = &resume_blob {
                    resume_session_tree(&state, blob, &root_token)?;
                    native_agent::retry_pending_closures(&state);
                    delegation::resume_delegations(
                        &state,
                        &blob.delegations,
                        Some(&input_lane_handle),
                        &root_token,
                    );
                }
                // ADR-0163 §5: a credential the previous process held and
                // this one did not carry is revoked; drop its socket file.
                delegation::sweep_stale_sockets(&state);
                state.with_mut(crate::state::ServerState::persist_first_start)?;
                let reconciled = state.with_mut(crate::state::ServerState::reconcile);
                if reconciled != crate::state::Reconciled::default() {
                    info!(
                        lost = reconciled.lost,
                        unobserved = reconciled.unobserved,
                        "reconciled activations a previous run left live"
                    );
                }
                let seed_missing = pre_seeded
                    .as_deref()
                    .filter(|name| state.with(|s| s.session_by_name(name).is_none()));
                if resume_blob.is_none()
                    && let Some(name) = seed_missing
                {
                    seed_initial_session(
                        &state,
                        name,
                        seed_with_pty,
                        seed_command,
                        scrollback,
                        &root_token,
                    );
                }
                // After the tree exists, so a restore lands beside the seed;
                // the saver dials the socket bound above.
                if let Some(autosave) = autosave {
                    crate::autosave::spawn(
                        autosave,
                        &state,
                        &root_token,
                        autosave_socket_path,
                        cold_start,
                    );
                }
                // Overlay detection shells out, so it runs inside the accept
                // set (see `serve_auto_overlay_listeners`); only the cheap gate
                // is evaluated here.
                let auto_overlay_ports = configured.unclaimed_overlay_ports();
                let auto_overlay_gate =
                    auto_overlay_allowed(auto_overlay_ports, &state, mode_unset);

                let mut accepts =
                    configured.accept_loops(&listener, &state, &root_token, &input_lane_handle);
                #[cfg(feature = "webtransport")]
                if let Some(wt) = &webtransport_listener {
                    accepts.push(Box::pin(accept_loop(
                        wt,
                        state.clone(),
                        root_token.clone(),
                        Some(input_lane_handle.clone()),
                    )));
                }
                accepts.push(Box::pin(serve_auto_overlay_listeners(
                    auto_overlay_gate,
                    auto_overlay_ports,
                    overlay_detect,
                    state.clone(),
                    root_token.clone(),
                    input_lane_handle.clone(),
                )));
                let startup_task = plugin_startup.map(|cfg| {
                    crate::plugin_startup::install(&state, *cfg, startup_socket, root_token.clone())
                });
                let accept_result = drive_accept_loops(accepts, &root_token).await;
                // Stop sidecars before this future returns. Dropping the
                // LocalSet afterwards would abort the task mid-grace.
                if !root_token.is_cancelled() {
                    root_token.cancel();
                }
                if let Some(task) = startup_task {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(8), task).await;
                }
                accept_result
            })
            .await;

        // Drop the LocalSet first: its tasks may hold `InputLaneHandle`
        // clones, and `InputLane::drop` joins a thread that only exits once
        // every handle is gone. The reverse order deadlocks.
        drop(local);
        drop(input_lane);

        // Unlinking a socket another server now owns would strand that
        // server unreachable by path.
        unlink_socket_if_ours(&socket_path, bound_socket);

        result
    }
}
