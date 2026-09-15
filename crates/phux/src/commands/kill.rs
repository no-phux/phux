use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_client::kill::{KeyedError, KillOutcome, ShutdownOutcome};
use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::info::SessionSnapshot;
use phux_server::runtime::default_socket_path;

use crate::commands::partial;
use crate::commands::server_target::{ServerSpec, ServerTarget};
use crate::commands::{cli_runtime, report_no_server, warn_interleaved_degradation};
use crate::selector;

/// Why `kill --server` refuses `--remote`, and what to do instead.
///
/// Not a client-side preference: the server accepts `SHUTDOWN` on its local
/// socket alone and answers a remote connection with `PermissionDenied`
/// (`handle_shutdown`). Refusing here, before any dial, names the rule
/// instead of surfacing it as a server refusal after a pairing round-trip.
const REMOTE_SHUTDOWN_REFUSAL: &str = "phux: `kill --server` is local-socket only: the server \
     accepts SHUTDOWN on its own socket and refuses it from a remote connection.\n  \
     stop it on that host (`ssh HOST phux kill --server`), or end its sessions with \
     `phux kill --remote HOST NAME`";

/// `phux kill` as the CLI parsed it: a selector, or `--server`.
///
/// `key` is `--idempotency-key` (`docs/spec/L1.md` §5.1.1); clap refuses it
/// beside `--server`.
pub(crate) fn run(
    target: Option<String>,
    stop_server: bool,
    key: Option<IdempotencyKey>,
    server: ServerSpec,
) -> ExitCode {
    if stop_server {
        return run_kill_server_spec(server);
    }
    // No target and no `--server` is unreachable: clap's `kill_what` group
    // is `required(true)`.
    target.map_or(ExitCode::FAILURE, |target| run_kill(&target, key, server))
}

/// `--server` against the local socket; a `--remote` is refused before any
/// dial (see [`REMOTE_SHUTDOWN_REFUSAL`]).
fn run_kill_server_spec(server: ServerSpec) -> ExitCode {
    if server.remote.is_some() {
        eprintln!("{REMOTE_SHUTDOWN_REFUSAL}");
        return ExitCode::from(2);
    }
    run_kill_server(server.socket)
}

/// `phux kill --server` — stop the running server, ending every session.
///
/// The stop is a wire command, not a signal, and that is the whole point.
/// A signal-killed server exits non-zero-equivalent, and launchd's
/// `KeepAlive{SuccessfulExit: false}` restarts it after `ThrottleInterval` --
/// so a signal-based stop would contradict the very promise ADR-0080 makes
/// ("a deliberately stopped server stays stopped") on the platform phux
/// mostly runs on. `SHUTDOWN` cancels the server's root token and it exits 0,
/// which is what makes the promise true (phux-pimp).
///
/// Exit codes: 0 when the server stopped (or was already gone -- this is
/// idempotent, because "make it not be running" is the caller's actual
/// intent), 1 when it could not be reached, 2 when it refused.
pub(crate) fn run_kill_server(socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    // Nothing listening is success, not an error: the caller asked for the
    // server to be stopped, and it is. Reap a stale entry on the way past so
    // the next auto-spawn does not have to.
    match phux_config::socket::probe(&socket_path) {
        phux_config::socket::SocketState::Absent => {
            eprintln!("phux: no server running at {}", socket_path.display());
            return ExitCode::SUCCESS;
        }
        phux_config::socket::SocketState::Stale => {
            let _ = phux_config::socket::reap_stale(&socket_path);
            eprintln!(
                "phux: no server running at {} (reaped a stale socket)",
                socket_path.display()
            );
            return ExitCode::SUCCESS;
        }
        phux_config::socket::SocketState::Live => {}
    }

    rt.block_on(async move {
        let mut conn = match ServerTarget::local(&socket_path).connect().await {
            Ok(conn) => conn,
            Err(err) => return report_no_server(&err, &socket_path, "kill --server"),
        };

        let result = phux_client::kill::shutdown(&mut conn, 1).await;
        drop(conn);
        match result {
            // The server acks then tears down, so losing the connection at
            // any point after the request is the expected shape, not a fault.
            Ok((ShutdownOutcome::Ok, degradation)) => warn_interleaved_degradation(&degradation),
            Err(AttachError::Disconnected) => {}
            Ok((ShutdownOutcome::Refused { code, message }, degradation)) => {
                warn_interleaved_degradation(&degradation);
                eprintln!("phux: server refused to stop ({code:?}): {message}");
                return ExitCode::from(2);
            }
            Ok((ShutdownOutcome::Unexpected(other), degradation)) => {
                warn_interleaved_degradation(&degradation);
                eprintln!("phux: unexpected reply to SHUTDOWN: {other:?}");
                return ExitCode::from(2);
            }
            Err(err) => {
                eprintln!("phux: kill --server failed: {err}");
                return ExitCode::FAILURE;
            }
        }

        // Wait for the socket to stop answering, not merely for the ack: the
        // caller's next act is often to start a replacement, and returning
        // while the old server still holds the socket makes that fail.
        let deadline = std::time::Instant::now() + SHUTDOWN_DEADLINE;
        while std::time::Instant::now() < deadline {
            if phux_config::socket::probe(&socket_path) != phux_config::socket::SocketState::Live {
                let _ = phux_config::socket::reap_stale(&socket_path);
                eprintln!("phux: server stopped");
                return ExitCode::SUCCESS;
            }
            tokio::time::sleep(SHUTDOWN_POLL).await;
        }
        eprintln!(
            "phux: server acknowledged the stop but is still listening on {} after {}s",
            socket_path.display(),
            SHUTDOWN_DEADLINE.as_secs()
        );
        ExitCode::FAILURE
    })
}

/// How long `--server` waits for the socket to stop answering after the ack.
///
/// Generous: teardown SIGHUPs every pane's process group and reaps each
/// child, so a server holding many panes legitimately takes longer than one
/// holding none. A caller that wants to start a replacement needs the socket
/// actually free, so exiting early would just move the failure.
const SHUTDOWN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Poll cadence while waiting for the socket to go quiet.
const SHUTDOWN_POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// `phux kill TARGET` — resolve the selector client-side, then ask the
/// server to tear it down. A whole-session target (`.` or a bare
/// `name`) resolves to its full Terminal-id list and rides a single
/// `KILL_RESOURCES { ids }` round-trip — the atomic multi-terminal op the
/// v0.3.0 "Option B" re-tier (ADR-0019 / ADR-0027) put in place of the
/// dissolved `KILL_COLLECTION` verb. A window / pane / `@id` target falls
/// back to one `KILL_RESOURCE` per resolved Terminal. Exit codes: 0 on
/// success, 1 on a selector miss / no server, 2 on a server-side refusal, 3
/// when a miss cannot be trusted because the hub could not see the whole
/// fleet (see [`partial`]).
///
/// `server` is the local socket or a `--remote` host (see `server_target`);
/// the selector resolves against that server's snapshot either way.
///
/// With `key`, the kill is one keyed command (`KILL_RESOURCE` for one
/// Terminal, `KILL_RESOURCES` for several), so a retry under the same key
/// answers the first result instead of killing again (L1 §5.1.1). An `@N` or
/// `host/@N` target is then sent as written, without a snapshot lookup, so a
/// retry after the first attempt removed the pane still reaches the server.
pub(crate) fn run_kill(target: &str, key: Option<IdempotencyKey>, server: ServerSpec) -> ExitCode {
    let selector = match selector::parse(target) {
        Ok(sel) => sel,
        Err(err) => {
            eprintln!("phux: invalid target '{target}': {err}");
            return ExitCode::FAILURE;
        }
    };
    let (rt, server) = match server.prepare("kill", false) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    rt.block_on(kill_selected(target, &selector, key, &server))
}

/// Resolve `selector` against a fresh snapshot of `server` and kill what it
/// names.
async fn kill_selected(
    target: &str,
    selector: &selector::Selector,
    key: Option<IdempotencyKey>,
    server: &ServerTarget,
) -> ExitCode {
    let mut conn = match server.connect().await {
        Ok(conn) => conn,
        Err(err) => return server.report_unreachable(false, &err, "kill"),
    };
    if key.is_some() && !phux_client::kill::keyed_signal_supported(&conn) {
        return crate::commands::spawn::unsupported_server(false, ServerFeature::KeyedSignal);
    }
    if let (Some(key), Some(id)) = (key, explicit_id(selector)) {
        return kill_keyed(&mut conn, server, target, vec![id], key).await;
    }

    // Resolve the selector against a fresh snapshot, keeping what that
    // snapshot could not see: `kill` acts on what the search finds, so an
    // empty result has to be told apart from an unsearchable fleet.
    let (snapshot, degradation) = match phux_client::state::get_state_on(&mut conn).await {
        Ok(view) => view.into_parts(),
        Err(err) => return server.report_unreachable(false, &err, "kill"),
    };

    // A whole-session target tears down in one round-trip via
    // KILL_RESOURCES { ids } — the atomic multi-terminal op the v0.3.0
    // "Option B" re-tier put in place of the dissolved KILL_COLLECTION
    // verb (ADR-0019 / ADR-0027). Grouping is now client logic: we
    // resolve the session to its full pane-id list and the server tears
    // them down together under its single state lock. Window / pane /
    // @id selectors address a strict subset and stay on the per-pane
    // KILL_RESOURCE path below.
    if let Some(session_name) = selector::whole_session_name(selector, &snapshot) {
        let ids = selector::resolve(selector, &snapshot);
        if ids.is_empty() && session_is_empty(&snapshot, &session_name) {
            return kill_empty_session(&mut conn, server, &session_name).await;
        }
        if ids.is_empty() {
            // A named session is hub-local by construction —
            // `handle_get_state_federated` discards a satellite's
            // `sessions` and `windows` because their `u32` ids would
            // collide — so a session that resolved to a name and then to
            // no panes is genuinely empty, degraded fleet or not.
            eprintln!("phux: no such target: {target}");
            return ExitCode::FAILURE;
        }
        // The session's own panes are all hub-local, but tearing one down
        // while half the fleet is invisible is still worth saying out
        // loud: the user asked to kill "everything named X".
        partial::warn_partial_view("kill", &degradation);
        return kill_whole_session(&mut conn, server, &session_name, ids, key).await;
    }

    let terminals = resolve_terminals(&mut conn, selector, &snapshot).await;
    if terminals.is_empty() {
        // `#tag` and `@id` selectors search `panes`, the one list a hub
        // *does* aggregate. Against an unreachable satellite, "nothing
        // matched" may mean "I could not look there" — and telling a user
        // their pane is gone when it is merely out of sight invites
        // exactly the wrong recovery.
        return partial::report_target_miss(Some(target), &degradation);
    }
    // A hit under degradation is still narrower than the user asked for:
    // `#tag` would have matched more panes with the fleet whole.
    partial::warn_partial_view("kill", &degradation);
    let code = match key {
        Some(key) => kill_keyed(&mut conn, server, target, terminals, key).await,
        None => kill_each_terminal(&mut conn, terminals).await,
    };
    drop(conn);
    code
}

/// The one id an explicit `@N` / `host/@N` target names.
fn explicit_id(selector: &selector::Selector) -> Option<ResourceId> {
    match selector {
        selector::Selector::ResourceId(id) => Some(ResourceId::local(*id)),
        selector::Selector::SatelliteResourceId { host, id } => {
            Some(ResourceId::satellite(host.as_str(), *id))
        }
        _ => None,
    }
}

/// Kill `terminals` as one keyed command: `KILL_RESOURCE` for one,
/// `KILL_RESOURCES` for several, so the key names the whole operation.
async fn kill_keyed(
    conn: &mut Connection,
    server: &ServerTarget,
    target: &str,
    mut terminals: Vec<ResourceId>,
    key: IdempotencyKey,
) -> ExitCode {
    let reply = if terminals.len() == 1 {
        let terminal_id = terminals.remove(0);
        phux_client::kill::kill_resource_keyed(conn, 1, terminal_id, key).await
    } else {
        phux_client::kill::kill_resources_keyed(conn, 1, terminals, key).await
    };
    report_batch_kill(reply, server, &format!("{target:?}"))
}

/// The Terminals a non-session selector names. A `#tag` selector resolves
/// against L3 tag metadata fetched on this same connection; every other form
/// is pure snapshot resolution.
async fn resolve_terminals(
    conn: &mut Connection,
    selector: &selector::Selector,
    snapshot: &SessionSnapshot,
) -> Vec<ResourceId> {
    if matches!(selector, selector::Selector::Tag(_)) {
        let index = phux_client::state::fetch_tag_index(conn, snapshot).await;
        return selector::resolve_with_tags(selector, snapshot, &index);
    }
    selector::resolve(selector, snapshot)
}

/// Whether the session named `name` holds no windows (ADR-0105).
fn session_is_empty(snapshot: &SessionSnapshot, name: &str) -> bool {
    snapshot
        .sessions
        .iter()
        .any(|session| session.name == name && session.is_empty())
}

/// Kill an empty session (ADR-0105).
///
/// It has no pane for `KILL_RESOURCES` to name, so the kill clears its
/// keep-empty mark, which makes the server remove a session holding no
/// windows. `SET_METADATA` has no reply, so a `GET_STATE` on the same ordered
/// connection confirms the session is gone. A disconnect in its place means
/// the server self-exited after its last session went, which is success.
async fn kill_empty_session(
    conn: &mut Connection,
    server: &ServerTarget,
    session_name: &str,
) -> ExitCode {
    if let Err(err) = phux_client::kill::clear_session_keep_empty(conn, 1, session_name).await {
        return server.report_unreachable(false, &err, "kill");
    }
    match phux_client::state::get_state_on(conn).await {
        Ok(view)
            if view
                .snapshot()
                .sessions
                .iter()
                .any(|s| s.name == session_name) =>
        {
            eprintln!("phux: kill refused for session {session_name:?}: the server kept it");
            ExitCode::from(2)
        }
        Ok(_) | Err(AttachError::Disconnected) => ExitCode::SUCCESS,
        Err(err) => server.report_unreachable(false, &err, "kill"),
    }
}

/// Send one `KILL_RESOURCES` for a whole session and report its outcome.
async fn kill_whole_session(
    conn: &mut Connection,
    server: &ServerTarget,
    session_name: &str,
    ids: Vec<ResourceId>,
    key: Option<IdempotencyKey>,
) -> ExitCode {
    let reply = match key {
        Some(key) => phux_client::kill::kill_resources_keyed(conn, 1, ids, key).await,
        None => phux_client::kill::kill_resources(conn, 1, ids)
            .await
            .map_err(KeyedError::from),
    };
    report_batch_kill(reply, server, &format!("session {session_name:?}"))
}

/// Report one kill round trip that covered a whole target: exit 0 when it
/// killed, 2 when the server refused (naming `label`), and the transport
/// error otherwise.
fn report_batch_kill(
    reply: Result<(KillOutcome, phux_client::state::Degradation), KeyedError>,
    server: &ServerTarget,
    label: &str,
) -> ExitCode {
    match reply {
        // `Killed` is the ack; a clean disconnect means the server
        // self-exited after its last session was reaped (phux-60s),
        // so the session is already gone — both are success.
        Ok((KillOutcome::Killed, degradation)) => {
            warn_interleaved_degradation(&degradation);
            ExitCode::SUCCESS
        }
        Err(KeyedError::Attach(AttachError::Disconnected)) => ExitCode::SUCCESS,
        Err(KeyedError::Unsupported) => {
            crate::commands::spawn::unsupported_server(false, ServerFeature::KeyedSignal)
        }
        Ok((KillOutcome::Refused(message), degradation)) => {
            warn_interleaved_degradation(&degradation);
            eprintln!("phux: kill refused for {label}: {message}");
            ExitCode::from(2)
        }
        Ok((KillOutcome::Unexpected(other), degradation)) => {
            warn_interleaved_degradation(&degradation);
            eprintln!(
                "phux: {label}: {}",
                phux_client::explain::explain_unexpected("kill", &other)
            );
            ExitCode::from(2)
        }
        Err(KeyedError::Attach(err)) => server.report_unreachable(false, &err, "kill"),
    }
}

/// How one `KILL_RESOURCE` ended.
enum KillStep {
    /// The server acknowledged the kill.
    Killed,
    /// The server refused it, or the reply could not confirm it. Reported.
    Refused,
    /// The server self-exited after its last session was reaped (phux-60s):
    /// every remaining target is already gone.
    ServerGone,
}

/// Kill each Terminal in turn; exit 2 if any kill was refused.
async fn kill_each_terminal(conn: &mut Connection, terminals: Vec<ResourceId>) -> ExitCode {
    let mut refused = false;
    for (i, terminal_id) in terminals.into_iter().enumerate() {
        let request_id = u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1);
        match kill_one(conn, request_id, terminal_id).await {
            KillStep::Killed => {}
            KillStep::Refused => refused = true,
            KillStep::ServerGone => break,
        }
    }
    if refused {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

/// Send one `KILL_RESOURCE`, reporting a refusal or failure on stderr.
async fn kill_one(conn: &mut Connection, request_id: u32, terminal_id: ResourceId) -> KillStep {
    let label = crate::selector::format_terminal_id(&terminal_id);
    match phux_client::kill::kill_resource(conn, request_id, terminal_id).await {
        Ok((KillOutcome::Killed, degradation)) => {
            warn_interleaved_degradation(&degradation);
            KillStep::Killed
        }
        Ok((KillOutcome::Refused(message), degradation)) => {
            warn_interleaved_degradation(&degradation);
            eprintln!("phux: kill refused for {label}: {message}");
            KillStep::Refused
        }
        Ok((KillOutcome::Unexpected(other), degradation)) => {
            warn_interleaved_degradation(&degradation);
            eprintln!(
                "phux: {label}: {}",
                phux_client::explain::explain_unexpected("kill", &other)
            );
            KillStep::Refused
        }
        // A clean disconnect means the server self-exited after its last
        // session was reaped (phux-60s): the remaining target Terminals are
        // already gone, so this is success, not failure.
        Err(AttachError::Disconnected) => KillStep::ServerGone,
        Err(err) => {
            eprintln!("phux: kill failed for {label}: {err}");
            KillStep::Refused
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{REMOTE_SHUTDOWN_REFUSAL, run};
    use crate::commands::server_target::ServerSpec;

    /// `kill --server --remote` is refused before any dial, with exit 2 and
    /// the reason named: the server only accepts SHUTDOWN locally.
    #[test]
    fn kill_server_refuses_a_remote_target_before_dialing() {
        let spec = ServerSpec {
            socket: None,
            remote: Some("mini".to_owned()),
        };
        assert_eq!(run(None, true, None, spec), std::process::ExitCode::from(2));
        assert!(REMOTE_SHUTDOWN_REFUSAL.contains("local-socket only"));
        assert!(REMOTE_SHUTDOWN_REFUSAL.contains("phux kill --remote HOST NAME"));
    }
}
