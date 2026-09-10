use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{Command as WireCommand, CommandResult};
use phux_protocol::wire::info::SessionSnapshot;
use phux_server::runtime::default_socket_path;

use crate::commands::server_target::{ServerSpec, ServerTarget};
use crate::commands::{cli_runtime, command_on, partial, report_no_server};
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
pub(crate) fn run(target: Option<String>, stop_server: bool, server: ServerSpec) -> ExitCode {
    if stop_server {
        return run_kill_server_spec(server);
    }
    // No target and no `--server` is unreachable: clap's `kill_what` group
    // is `required(true)`.
    target.map_or(ExitCode::FAILURE, |target| run_kill(&target, server))
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
        let mut conn = match Connection::connect(&socket_path).await {
            Ok(conn) => conn,
            Err(err) => return report_no_server(&err, &socket_path, "kill --server"),
        };

        let result = command_on(&mut conn, 1, WireCommand::Shutdown).await;
        match result {
            // The server acks then tears down, so losing the connection at
            // any point after the request is the expected shape, not a fault.
            Ok(CommandResult::Ok) | Err(AttachError::Disconnected) => {}
            Ok(CommandResult::Error { code, message }) => {
                eprintln!("phux: server refused to stop ({code:?}): {message}");
                return ExitCode::from(2);
            }
            Ok(other) => {
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
pub(crate) fn run_kill(target: &str, server: ServerSpec) -> ExitCode {
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
    rt.block_on(kill_selected(target, &selector, &server))
}

/// Resolve `selector` against a fresh snapshot of `server` and kill what it
/// names.
async fn kill_selected(
    target: &str,
    selector: &selector::Selector,
    server: &ServerTarget,
) -> ExitCode {
    let mut conn = match server.connect().await {
        Ok(conn) => conn,
        Err(err) => return server.report_unreachable(false, &err, "kill"),
    };

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
        let command = WireCommand::KillResources { ids };
        return kill_whole_session(&mut conn, server, &session_name, command).await;
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
    kill_each_terminal(&mut conn, terminals).await
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

/// Send one `KILL_RESOURCES` for a whole session and report its outcome.
async fn kill_whole_session(
    conn: &mut Connection,
    server: &ServerTarget,
    session_name: &str,
    command: WireCommand,
) -> ExitCode {
    match command_on(conn, 1, command).await {
        // `Ok` is the ack; a clean disconnect means the server
        // self-exited after its last session was reaped (phux-60s),
        // so the session is already gone — both are success.
        Ok(CommandResult::Ok) | Err(AttachError::Disconnected) => ExitCode::SUCCESS,
        Ok(CommandResult::Error { message, .. }) => {
            eprintln!("phux: kill refused for session {session_name:?}: {message}");
            ExitCode::from(2)
        }
        Ok(other) => {
            eprintln!(
                "phux: session {session_name:?}: {}",
                phux_client::explain::explain_unexpected("kill", &other)
            );
            ExitCode::from(2)
        }
        Err(err) => server.report_unreachable(false, &err, "kill"),
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
    match command_on(conn, request_id, WireCommand::KillResource { terminal_id }).await {
        Ok(CommandResult::Ok) => KillStep::Killed,
        Ok(CommandResult::Error { message, .. }) => {
            eprintln!("phux: kill refused for {label}: {message}");
            KillStep::Refused
        }
        Ok(other) => {
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
        assert_eq!(run(None, true, spec), std::process::ExitCode::from(2));
        assert!(REMOTE_SHUTDOWN_REFUSAL.contains("local-socket only"));
        assert!(REMOTE_SHUTDOWN_REFUSAL.contains("phux kill --remote HOST NAME"));
    }
}
