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
        s.flush_reports_before_shutdown();
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
        crate::runtime::connections::detach_and_release_consumer_state(state, client_id);
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

    match crate::runtime::apps::terminal::launch::seed_named_session(
        state, name, command, cwd, env, origin, root_token,
    ) {
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    use phux_core::{Actor, ExitOutcome, ExitPolicy, SessionLifetime, TerminalLaunch};
    use phux_protocol::policy::{PeerIdentity, TransportType};
    use phux_protocol::wire::frame::Command;
    use tracing_subscriber::fmt::MakeWriter;

    use super::super::routing::{CommandContext, handle_command};
    use crate::org_store::MemOrgStore;
    use crate::state::{ServerState, TerminalTarget};

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buf {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Buf {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// `SHUTDOWN` offers a still-pending activation report once more, then
    /// warns, before `shutting_down` makes later reports a silent no-op.
    #[test]
    fn shutdown_retries_a_pending_activation_report_and_warns() {
        let state = crate::state::SharedState::new();
        let client = state.with_mut(|s| {
            let store = MemOrgStore::default();
            let fail = store.fail_commits();
            s.set_org_store(Box::new(store));
            let (_, id) = s
                .create_terminal(
                    TerminalTarget::NewSession {
                        name: "kept",
                        lifetime: SessionLifetime::Persistent,
                    },
                    TerminalLaunch::default(),
                    ExitPolicy::Retain,
                    &Actor::owner(),
                )
                .expect("create");
            fail.store(true, Ordering::Relaxed);
            s.activation_ended(id, ExitOutcome::UNKNOWN, true);
            assert!(
                s.has_pending_activation_reports(),
                "the refused end stays pending"
            );
            ServerState::reset_executor_persist_attempts();
            let client = s.new_client_id();
            s.set_peer_identity(
                client,
                PeerIdentity {
                    uid: 1,
                    pid: None,
                    exe_path: None,
                    mcp_host_key: None,
                    transport: TransportType::UnixSocket,
                    source_addr: None,
                },
            );
            client
        });

        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(4);
        let ctx = CommandContext::for_test(&state, client, &out_tx);
        let buf = Buf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(buf.clone())
            .finish();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tracing::subscriber::with_default(subscriber, || {
            tracing::callsite::rebuild_interest_cache();
            runtime.block_on(handle_command(&ctx, 1, Command::Shutdown));
        });
        out_rx.try_recv().expect("SHUTDOWN acks");

        assert_eq!(
            ServerState::executor_persist_attempts(),
            1,
            "SHUTDOWN must offer the pending report once more"
        );
        let log = String::from_utf8(buf.0.lock().expect("log").clone()).unwrap();
        assert!(
            log.contains(
                "shutdown with undurable executor evidence; restart can only reconcile committed state"
            ),
            "{log}"
        );
        assert!(state.with(ServerState::shutting_down));
        assert!(state.with(ServerState::has_pending_activation_reports));
    }
}
