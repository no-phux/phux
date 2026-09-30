use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::kill::{KillError, ShutdownOutcome};
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::IdempotencyKey;
use phux_server::runtime::default_socket_path;

use crate::commands::server_target::{ServerSpec, ServerTarget};
use crate::commands::{cli_runtime, report_no_server, warn_interleaved_degradation};
use crate::commands::{confirm, partial};
use phux_client::selector;

/// Why `kill --server` refuses `--remote`: the server accepts `SHUTDOWN` on
/// its local socket only, so refuse before dialing.
const REMOTE_SHUTDOWN_REFUSAL: &str = "phux: `kill --server` is local-socket only: the server \
     accepts SHUTDOWN on its own socket and refuses it from a remote connection.\n  \
     stop it on that host (`ssh HOST phux kill --server`), or end its sessions with \
     `phux kill --remote HOST NAME`";

/// `phux kill` as parsed: a selector or `--server`. `key` is
/// `--idempotency-key` (L1 §5.1.1). A selector kill is dangerous (ADR-0128) and
/// needs `yes` or a typed "y"; `--server` never asks.
pub(crate) fn run(
    target: Option<String>,
    stop_server: bool,
    key: Option<IdempotencyKey>,
    yes: bool,
    server: ServerSpec,
) -> ExitCode {
    if stop_server {
        return run_kill_server_spec(server);
    }
    // No target and no `--server` is unreachable: TARGET is
    // `required_unless = "--server"`, so the parser refuses it first.
    target.map_or(ExitCode::FAILURE, |target| {
        run_kill(&target, key, server, |action| {
            confirm::confirmed(yes, action)
        })
    })
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
/// A `SHUTDOWN` wire command rather than a signal, so the server exits 0 and
/// launchd's `KeepAlive{SuccessfulExit: false}` leaves it stopped (ADR-0080).
/// Exit 0 when stopped or already gone (idempotent), 1 when unreachable, 2 when
/// refused.
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

/// How long `--server` waits for the socket to stop answering after the ack;
/// teardown reaps every pane, and a replacement needs the socket free.
const SHUTDOWN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Poll cadence while waiting for the socket to go quiet.
const SHUTDOWN_POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// `phux kill TARGET` — resolve the selector client-side, then kill. A
/// whole-session target rides one `KILL_RESOURCES { ids }`; others send one
/// `KILL_RESOURCE` per Terminal. Exit 0 success, 1 miss / no server, 2 refusal,
/// 3 an untrustworthy miss on a partial fleet (see [`partial`]).
///
/// With `key` the kill is one keyed command, so a retry answers the first
/// result (L1 §5.1.1); an `@N` target is then sent as written, without a
/// lookup. `confirm` runs after validation and before any dial.
pub(crate) fn run_kill(
    target: &str,
    key: Option<IdempotencyKey>,
    server: ServerSpec,
    confirm: impl FnOnce(&str) -> Result<(), ExitCode>,
) -> ExitCode {
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
    if let Err(code) = confirm(&format!("kill {target}")) {
        return code;
    }
    rt.block_on(kill_selected(target, &selector, key, &server))
}

/// Resolve `selector` against a fresh snapshot of `server` and kill what it
/// names. Orchestration lives in [`phux_client::kill::selected`].
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
    let mut notices = Vec::new();
    let result = phux_client::kill::selected(&mut conn, selector, target, key, &mut notices).await;
    drop(conn);
    report_selected(result, &notices, server)
}

/// Print snapshot and interleaved notices, then map [`KillError`] onto the
/// CLI's exit codes.
fn report_selected(
    result: Result<phux_client::kill::Selected, KillError>,
    notices: &[String],
    server: &ServerTarget,
) -> ExitCode {
    for notice in notices {
        eprintln!(
            "{}",
            phux_client::state::partial_view_warning("kill", notice)
        );
    }
    match result {
        Ok(done) => {
            for message in done.interleaved {
                eprintln!("phux: warning: partial results — {message}");
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            for message in err.interleaved() {
                eprintln!("phux: warning: partial results — {message}");
            }
            match err {
                KillError::UnsupportedKeyedSignal => {
                    crate::commands::spawn::unsupported_server(false, ServerFeature::KeyedSignal)
                }
                KillError::NoSuchTarget { target } => {
                    eprintln!("phux: no such target: {target}");
                    eprintln!("  run `phux ls` to see live sessions and panes");
                    ExitCode::FAILURE
                }
                KillError::Unresolved {
                    target,
                    degradation,
                } => partial::report_target_miss(Some(&target), &degradation),
                KillError::Refused { label, message, .. } => {
                    eprintln!("phux: kill refused for {label}: {message}");
                    ExitCode::from(2)
                }
                KillError::Unexpected { label, message, .. } => {
                    eprintln!("phux: {label}: {message}");
                    ExitCode::from(2)
                }
                KillError::PaneRefusals { messages, .. } => {
                    for message in messages {
                        eprintln!("phux: {message}");
                    }
                    ExitCode::from(2)
                }
                KillError::SessionKept { session } => {
                    eprintln!("phux: kill refused for session {session:?}: the server kept it");
                    ExitCode::from(2)
                }
                KillError::Attach(err) => server.report_unreachable(false, &err, "kill"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{REMOTE_SHUTDOWN_REFUSAL, run, run_kill};
    use crate::commands::confirm;
    use crate::commands::server_target::ServerSpec;

    /// `kill --server --remote` is refused before any dial, with exit 2 and
    /// the reason named: the server only accepts SHUTDOWN locally.
    #[test]
    fn kill_server_refuses_a_remote_target_before_dialing() {
        let spec = ServerSpec {
            socket: None,
            remote: Some("mini".to_owned()),
        };
        assert_eq!(
            run(None, true, None, false, spec),
            std::process::ExitCode::from(2)
        );
        assert!(REMOTE_SHUTDOWN_REFUSAL.contains("local-socket only"));
        assert!(REMOTE_SHUTDOWN_REFUSAL.contains("phux kill --remote HOST NAME"));
    }

    /// ADR-0128: a kill needs `--yes` when nobody can be asked. Without it,
    /// on a non-terminal stdin, the verb exits 2 before it dials: the
    /// server's socket never sees a connection.
    #[test]
    fn kill_without_yes_on_a_non_tty_exits_2_and_sends_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let spec = ServerSpec {
            socket: Some(socket),
            remote: None,
        };
        let code = run_kill("@1", None, spec, |action| {
            confirm::consent(false, false, action, || None)
        });
        assert_eq!(code, std::process::ExitCode::from(confirm::NOT_CONFIRMED));
        assert!(
            matches!(listener.accept(), Err(err) if err.kind() == std::io::ErrorKind::WouldBlock),
            "nothing dialed the server"
        );
    }
}
