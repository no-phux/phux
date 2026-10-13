use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::kill::{KillError, ShutdownOutcome};
use phux_client::org::{self, KillReport, SessionOutcome, TargetOutcome};
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
    verbose: bool,
    server: ServerSpec,
) -> ExitCode {
    if stop_server {
        return run_kill_server_spec(server);
    }
    // No target and no `--server` is unreachable: TARGET is
    // `required_unless = "--server"`, so the parser refuses it first.
    target.map_or(ExitCode::FAILURE, |target| {
        run_kill(&target, key, verbose, server, |action| {
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
/// whole-session target runs the kill workflow against a server that
/// advertises `ORGANIZATION` (terminate every member, remove each once
/// inactive, then delete the session) and prints one result line per member;
/// against an older server it rides one `KILL_RESOURCES { ids }`. The
/// session report is one summary line plus a line per member that was not
/// removed; `verbose` prints every member ([`report_lines`]). Other
/// targets send one `KILL_RESOURCE` per Terminal. Exit 0 success, 1 miss / no
/// server, 2 refusal or an incomplete workflow, 3 an untrustworthy miss on a
/// partial fleet (see [`partial`]).
///
/// With `key` the kill is one keyed command, so a retry answers the first
/// result (L1 §5.1.1); an `@N` target is then sent as written, without a
/// lookup. `confirm` runs after validation and before any dial.
pub(crate) fn run_kill(
    target: &str,
    key: Option<IdempotencyKey>,
    verbose: bool,
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
    rt.block_on(kill_selected(target, &selector, key, verbose, &server))
}

/// Resolve `selector` against a fresh snapshot of `server` and kill what it
/// names. Orchestration lives in [`phux_client::kill::selected`].
async fn kill_selected(
    target: &str,
    selector: &selector::Selector,
    key: Option<IdempotencyKey>,
    verbose: bool,
    server: &ServerTarget,
) -> ExitCode {
    let mut conn = match server.connect().await {
        Ok(conn) => conn,
        Err(err) => return server.report_unreachable(false, &err, "kill"),
    };
    let mut notices = Vec::new();
    let result = phux_client::kill::selected(&mut conn, selector, target, key, &mut notices).await;
    drop(conn);
    let report = Report { target, verbose };
    report_selected(result, &notices, server, report)
}

/// How a session kill's report reads: which target it names, and whether
/// every member gets a line.
#[derive(Clone, Copy)]
struct Report<'a> {
    target: &'a str,
    verbose: bool,
}

/// Print snapshot and interleaved notices, then map [`KillError`] onto the
/// CLI's exit codes.
fn report_selected(
    result: Result<phux_client::kill::Selected, KillError>,
    notices: &[String],
    server: &ServerTarget,
    how: Report<'_>,
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
            if let Some(report) = &done.report {
                out!("{}", report_lines(report, how));
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
                KillError::Workflow { ref report, .. } => {
                    out!("{}", report_lines(report, how));
                    eprintln!("phux: {err}");
                    ExitCode::from(2)
                }
                KillError::EmptySessionUnsupported { .. } => {
                    eprintln!("phux: {err}");
                    eprintln!("  upgrade the server, or kill its panes instead");
                    ExitCode::from(2)
                }
                KillError::Org(err) => {
                    eprintln!("phux: kill failed: {err}");
                    ExitCode::from(2)
                }
                KillError::Agent(err) => {
                    crate::commands::report_agent_resolve_error(false, &err, false)
                }
                KillError::Attach(err) => server.report_unreachable(false, &err, "kill"),
            }
        }
    }
}

/// A session kill's report: one summary line, `killed 12 terminals in
/// work`, then `^abc123  refused: ...` for each member that was not removed
/// and the session's own outcome when it was not deleted. `verbose` prints
/// every member and the session, keyed by the prefix `phux ls -l` shows.
fn report_lines(report: &KillReport, how: Report<'_>) -> String {
    use std::fmt::Write as _;
    let names = phux_client::kill::report_key_names(report);
    let removed = report
        .targets
        .iter()
        .filter(|(_, outcome)| gone(outcome))
        .count();
    let total = report.targets.len();
    let deleted = matches!(
        report.session_outcome,
        SessionOutcome::Deleted | SessionOutcome::AlreadyGone
    );
    let mut out = String::new();
    let terminals = if total == 1 { "terminal" } else { "terminals" };
    let target = how.target;
    let _ = if total == 0 {
        writeln!(out, "deleted empty session {target}")
    } else if removed == total {
        writeln!(out, "killed {total} {terminals} in {target}")
    } else {
        writeln!(out, "killed {removed} of {total} {terminals} in {target}")
    };
    for (key, outcome) in &report.targets {
        if how.verbose || !gone(outcome) {
            let _ = writeln!(
                out,
                "  {}  {}",
                names.display(*key.as_bytes()),
                target_word(outcome)
            );
        }
    }
    if how.verbose || !deleted {
        let _ = writeln!(
            out,
            "  session {}  {}",
            names.display(*report.session.as_bytes()),
            session_word(&report.session_outcome)
        );
    }
    out
}

/// Whether a member is out of the session, as asked.
const fn gone(outcome: &TargetOutcome) -> bool {
    matches!(outcome, TargetOutcome::Removed | TargetOutcome::AlreadyGone)
}

fn target_word(outcome: &TargetOutcome) -> String {
    match outcome {
        TargetOutcome::Removed => "removed".to_owned(),
        TargetOutcome::AlreadyGone => "already gone".to_owned(),
        TargetOutcome::Refused(refusal) => format!("refused: {}", org::refusal_sentence(refusal)),
        TargetOutcome::Unknown(reason) => format!("outcome unknown: {reason}"),
        TargetOutcome::TimedOut => "did not stop in time".to_owned(),
        TargetOutcome::NoLiveTarget => "runs on another server instance".to_owned(),
    }
}

fn session_word(outcome: &SessionOutcome) -> String {
    match outcome {
        SessionOutcome::Deleted => "deleted".to_owned(),
        SessionOutcome::AlreadyGone => "already gone".to_owned(),
        SessionOutcome::Kept => "kept (a member was not removed)".to_owned(),
        SessionOutcome::Refused(refusal) => {
            format!("delete refused: {}", org::refusal_sentence(refusal))
        }
        SessionOutcome::Unknown(reason) => format!("delete outcome unknown: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{REMOTE_SHUTDOWN_REFUSAL, Report, report_lines, run, run_kill};
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
            run(None, true, None, false, false, spec),
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
        let code = run_kill("@1", None, false, spec, |action| {
            confirm::consent(false, false, action, || None)
        });
        assert_eq!(code, std::process::ExitCode::from(confirm::NOT_CONFIRMED));
        assert!(
            matches!(listener.accept(), Err(err) if err.kind() == std::io::ErrorKind::WouldBlock),
            "nothing dialed the server"
        );
    }

    fn report(
        outcomes: Vec<phux_client::org::TargetOutcome>,
        session_outcome: phux_client::org::SessionOutcome,
    ) -> phux_client::org::KillReport {
        use phux_protocol::ids::{ResourceKey, SessionKey};
        let targets = outcomes
            .into_iter()
            .enumerate()
            .map(|(n, outcome)| {
                let byte = u8::try_from(n + 1).expect("few members");
                (ResourceKey::new([byte; 16]).expect("key"), outcome)
            })
            .collect();
        phux_client::org::KillReport {
            session: SessionKey::new([0xee; 16]).expect("key"),
            targets,
            session_outcome,
        }
    }

    /// A clean session kill is one line, however many members it ended.
    #[test]
    fn a_clean_kill_reports_one_summary_line() {
        use phux_client::org::{SessionOutcome, TargetOutcome};
        let report = report(vec![TargetOutcome::Removed; 12], SessionOutcome::Deleted);
        let how = Report {
            target: "work",
            verbose: false,
        };
        assert_eq!(report_lines(&report, how), "killed 12 terminals in work\n");
    }

    /// A partial kill summarizes, then names each member that was not
    /// removed and why the session was kept, keyed by the six-digit prefix
    /// `phux ls -l` shows. `-v` names every member.
    #[test]
    fn a_partial_kill_names_only_what_went_wrong_unless_verbose() {
        use phux_client::org::{SessionOutcome, TargetOutcome};
        use phux_protocol::wire::frame::Refusal;
        let report = report(
            vec![
                TargetOutcome::Removed,
                TargetOutcome::Refused(Refusal::ExecutionUncertain),
            ],
            SessionOutcome::Kept,
        );
        let quiet = report_lines(
            &report,
            Report {
                target: "work",
                verbose: false,
            },
        );
        let lines: Vec<&str> = quiet.lines().collect();
        assert_eq!(lines.len(), 3, "{quiet}");
        assert_eq!(lines[0], "killed 1 of 2 terminals in work");
        assert!(lines[1].starts_with("  ^"), "{quiet}");
        assert!(lines[1].contains("refused"), "{quiet}");
        assert!(lines[2].starts_with("  session ^"), "{quiet}");
        assert!(lines[2].contains("kept"), "{quiet}");

        let loud = report_lines(
            &report,
            Report {
                target: "work",
                verbose: true,
            },
        );
        let lines: Vec<&str> = loud.lines().collect();
        assert_eq!(lines.len(), 4, "{loud}");
        assert!(lines[1].ends_with("removed"), "{loud}");
    }
}
