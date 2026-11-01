//! Detach, shutdown, upgrade, and session creation.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;

/// Handle `DETACH_RESOURCE` (SPEC §5.1): drop every output task and the
/// event subscription the caller holds on the terminal. Idempotent, so a
/// detach never races a natural close into an error.
pub(crate) async fn handle_detach_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
) -> CommandResult {
    let handle = state.with_mut(|s| {
        s.unsubscribe_resource_events(client_id, terminal_id);
        // A declared `VIEWER` outlives the subscription (ADR-0127).
        let core = s
            .resource_from_wire(terminal_id)
            .or_else(|| s.draining_resource_from_wire(terminal_id))?;
        s.unsubscribe_resource(client_id, core);
        Some((core, s.resource_handle(core).cloned()))
    });
    let Some((core, handle)) = handle else {
        return CommandResult::Ok;
    };
    pump::stop_output(state, client_id, core).await;
    state.with_mut(|s| s.cancel_attach_terminal_pump(client_id, core));
    // Fence engine-emitted output before COMMAND_RESULT is queued.
    if let Some(handle) = &handle {
        crate::runtime::apps::app_for(handle.kind)
            .fence_consumer(handle, client_id)
            .await;
    }
    debug!(?client_id, ?terminal_id, "DETACH_RESOURCE unsubscribed");
    CommandResult::Ok
}

/// Handle `SHUTDOWN`: cancel the root token, the same shutdown idle-exit and
/// signals take, after acking. Accepted on the Unix socket only (L1 §5.1):
/// whether a remote peer may stop the host is an unanswered policy question.
pub(super) async fn handle_shutdown(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
) {
    let transport = state.with(|s| s.peer_identity(client_id).map(|peer| peer.transport));
    if !matches!(
        transport,
        Some(phux_protocol::policy::TransportType::UnixSocket)
    ) {
        warn!(
            ?client_id,
            ?transport,
            "SHUTDOWN refused: local socket only"
        );
        let refusal = CommandResult::Error {
            code: ErrorCode::PermissionDenied,
            message: "SHUTDOWN is accepted on the local socket only".to_owned(),
        };
        send_result(out_tx, request_id, refusal).await;
        return;
    }

    info!(?client_id, "SHUTDOWN requested; stopping the server");
    // ADR-0104 §4: record the close reason before the exit watchers run.
    state.with_mut(|s| {
        s.begin_shutdown();
        for resource in s.resource_ids() {
            s.mark_resource_closing(
                resource,
                phux_protocol::wire::frame::CloseReason::ServerShutdown,
            );
        }
    });
    send_result(out_tx, request_id, CommandResult::Ok).await;
    // Let the ack reach the writer before the teardown races it.
    tokio::task::yield_now().await;
    root_token.cancel();
}

/// Handle `UPGRADE` (ADR-0032): prepare the graceful re-exec, ack the client,
/// then replace the process. Acks itself (rather than returning a
/// `CommandResult`) because on success it never returns.
pub(super) async fn handle_upgrade(
    state: &SharedState,
    request_id: u32,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    let result = match super::super::upgrade::prepare_upgrade(state).await {
        Ok(plan) => {
            // Best-effort ack flushed before exec; the client reconnects.
            send_result(out_tx, request_id, CommandResult::Ok).await;
            tokio::task::yield_now().await;
            info!("UPGRADE: re-exec'ing the new binary");
            let err = plan.exec().await;
            // Only reached if exec failed: the old image keeps serving, so
            // the sidecars stopped for the exec come back.
            crate::plugin_startup::resume(state);
            error!(error = %err, "UPGRADE exec failed; continuing on the current image");
            return;
        }
        Err(err) => {
            warn!(error = %err, "UPGRADE preparation failed");
            CommandResult::Error {
                code: ErrorCode::InternalError,
                message: format!("upgrade failed: {err}"),
            }
        }
    };
    send_result(out_tx, request_id, result).await;
}

/// Force-detach the session-attached clients of `session` (all when `None`)
/// for `phux detach`, answering the count as JSON. An unknown session
/// detaches nobody. `ATTACH_RESOURCE` subscribers are not swept.
pub(crate) fn handle_detach_clients(state: &SharedState, session: Option<&str>) -> CommandResult {
    let targets = state.with(|s| s.attached_clients_to_detach(session));
    let count = targets.len();
    for (client_id, tx) in targets {
        // `try_send`: a wedged mailbox is the stuck client this verb exists to
        // clear, so never await its capacity; the teardown still runs.
        let _ = tx.try_send(Outbound::Frame(FrameKind::Detached {
            reason: Some(DetachReason::Requested),
            message: "detached by `phux detach`".to_owned(),
        }));
        super::super::client::detach_and_release_consumer_state(state, client_id);
    }
    debug!(
        ?session,
        count, "DETACH_CLIENTS: force-detached clients from outside the attach UI"
    );
    CommandResult::OkWith(CommandValue::Json(count.to_string()))
}

/// Create a named session and seed its pane without attaching: the
/// `SESSION_CREATE_KEY` metadata write (ADR-0019 / ADR-0027).
///
/// The existence check and seed run on the single-threaded runtime, so two
/// racing creates for one `name` cannot both succeed. Returns the seed pane's
/// wire id; `Err` is log-only because `SET_METADATA` has no reply frame.
pub(crate) fn create_named_session(
    state: &SharedState,
    name: &str,
    command: Option<Vec<String>>,
    cwd: Option<&str>,
    env: std::collections::BTreeMap<String, String>,
    origin: SeedOrigin,
    root_token: &CancellationToken,
) -> Result<phux_protocol::ids::ResourceId, String> {
    if state.with(|s| s.session_by_name(name).is_some()) {
        return Err(format!("session {name:?} already exists"));
    }

    let (with_pty, scrollback) =
        state.with(|s| (s.attach_create_seeds_pty(), s.scrollback_limits()));

    let seed_cmd = with_pty.then(|| {
        crate::runtime::apps::terminal::launch::named_session_command(
            state, name, command, cwd, env,
        )
    });
    match seed_session(state, name, seed_cmd, scrollback, root_token, None, origin) {
        Ok(core_terminal) => {
            // A headless create arms the last-session self-exit like an attach.
            let wire = state.with_mut(|s| {
                s.arm_self_exit();
                s.intern_resource_wire(core_terminal)
            });
            Ok(wire)
        }
        Err(err) => {
            warn!(
                session = %name,
                error = %err,
                "session-create: failed to seed pane for new session",
            );
            Err(format!("failed to create session {name:?}: {err}"))
        }
    }
}

/// Commit an empty persistent session, the `empty: true` form of the
/// `SESSION_CREATE_KEY` write, attributed to `actor`. It does not arm
/// self-exit: an empty session runs nothing, so the writer's disconnect
/// must not end the server it just created a session on.
pub(crate) fn create_empty_session(
    state: &SharedState,
    name: &str,
    actor: &phux_core::Actor,
) -> Result<(), String> {
    state.with_mut(|s| {
        if s.session_by_name(name).is_some() {
            return Err(format!("session {name:?} already exists"));
        }
        s.create_empty_session(name, phux_core::SessionLifetime::Persistent, actor)
            .map(|_| ())
            .map_err(|refusal| format!("failed to create session {name:?}: {refusal}"))
    })
}
