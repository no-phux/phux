use std::path::Path;

use phux_client::attach::AttachError;

use crate::commands::diagnostics::{attach_error_lines, no_server_lines};
use crate::commands::{parse_selector, sh_quote};
use phux_client::selector::Selector;

/// Every word is quoted, and an embedded single quote is escaped rather
/// than terminating the quoting.
#[test]
fn sh_quote_is_unconditional_and_closes_over_single_quotes() {
    assert_eq!(sh_quote("claude"), "'claude'");
    assert_eq!(sh_quote("/opt/my agent/claude"), "'/opt/my agent/claude'");
    assert_eq!(sh_quote("it's"), r"'it'\''s'");
    assert_eq!(sh_quote(""), "''");
    // The injection this exists to stop: nothing escapes the quotes.
    assert_eq!(sh_quote("; rm -rf /"), "'; rm -rf /'");
    assert_eq!(sh_quote("$(whoami)"), "'$(whoami)'");
}

fn refused_io() -> AttachError {
    AttachError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused))
}

/// The CLI spells every `%name` refusal with the same code the client,
/// the spatial verbs, and MCP report, and a partial index exits 3 except
/// behind the shared single-pane resolver, which keeps its status
/// (ADR-0075 point 3).
#[test]
fn agent_resolve_exit_matches_the_refusal_unless_status_is_kept() {
    use phux_client::selector::AgentResolveError;
    let t = phux_protocol::ids::ResourceId::local(1);
    for err in [
        AgentResolveError::Unknown { name: "a".into() },
        AgentResolveError::Ambiguous {
            name: "a".into(),
            candidates: vec![],
        },
        AgentResolveError::AmbiguousSession {
            name: "a".into(),
            terminal: t.clone(),
            candidates: vec![],
        },
        AgentResolveError::Withdrawn {
            name: "a".into(),
            terminal: t,
        },
        AgentResolveError::PartialIndex {
            name: "a".into(),
            matched: vec![],
        },
    ] {
        assert_eq!(
            super::targets::agent_resolve_exit(&err, false),
            err.exit_code()
        );
    }
    let partial = AgentResolveError::PartialIndex {
        name: "a".into(),
        matched: vec![],
    };
    assert_eq!(super::targets::agent_resolve_exit(&partial, false), 3);
    assert_eq!(super::targets::agent_resolve_exit(&partial, true), 1);
}

const SERVER_LOG: &str = "server log: /state/phux/server.log";
const DOCTOR: &str = "run `phux doctor` for a health check";

/// phux-i0e8.7.3: the no-server arm names the exact start commands, and
/// every arm ends with the server log and the doctor pointer. A
/// mid-command disconnect has no start command to suggest, and the
/// generic arm keeps the error's own sentence first.
#[test]
fn no_server_lines_name_their_cause_log_and_doctor() {
    let socket = Path::new("/tmp/phux-test.sock");
    let log = Path::new("/state/phux/server.log");
    let lines = |err: &AttachError, verb| no_server_lines(err, socket, verb, log, None);
    assert_eq!(
        lines(&refused_io(), "ls"),
        [
            "no server running at /tmp/phux-test.sock",
            "start one with `phux` (attaches, auto-starting a server) or `phux server --ensure`",
            SERVER_LOG,
            DOCTOR,
        ]
    );
    assert_eq!(
        lines(&AttachError::Disconnected, "kill"),
        [
            "server closed the connection during kill",
            SERVER_LOG,
            DOCTOR
        ]
    );
    assert_eq!(
        lines(&AttachError::Refused("policy said no".to_owned()), "tag"),
        [
            "tag failed: server refused the request: policy said no",
            SERVER_LOG,
            DOCTOR
        ]
    );
}

/// A socket that exists but cannot be dialed names the path and the OS
/// reason, not the client's internal "attach loop io error" wording.
#[test]
fn no_server_unreachable_socket_names_the_path_and_reason() {
    let denied = AttachError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    let socket = Path::new("/tmp/s.sock");
    let lines = no_server_lines(&denied, socket, "ls", Path::new("/log"), None);
    assert!(
        lines[0].starts_with("ls: cannot reach the server socket /tmp/s.sock: "),
        "{lines:?}"
    );
    assert!(!lines[0].contains("attach loop"), "{lines:?}");
    assert_eq!(lines[2], DOCTOR);

    let json = super::json_err::no_server_error(&denied, socket, "ls");
    assert_eq!(json.code, super::json_err::codes::TRANSPORT);
    assert!(json.message.contains("/tmp/s.sock"), "{}", json.message);
}

/// The three self-explaining attach arms stay single-line (`phux-roz`: the
/// no-server one is copy-pasteable). A disconnect that reaches the printer
/// without the reconnect window names the SERVER log, where the reason
/// lives (phux-i0e8.2.3); the fallthrough names this client's own log and
/// the client protocol triple, since a `Protocol` error usually means the
/// binaries disagree (phux-i0e8.7.3).
#[test]
fn attach_error_lines_name_their_cause_and_remedy() {
    let lines = |err: &AttachError| {
        attach_error_lines(
            err,
            Path::new("/tmp/a.sock"),
            "main",
            Path::new("/state/phux/client-42.log"),
            None,
        )
    };
    for (err, line) in [
        (
            refused_io(),
            "no server at /tmp/a.sock. Start one with: phux server --ensure",
        ),
        (
            AttachError::Refused("no such session".to_owned()),
            "server refused attach: no such session",
        ),
        (
            AttachError::NotATty,
            "attach requires an interactive terminal (stdin is not a TTY).",
        ),
    ] {
        assert_eq!(lines(&err), [line]);
    }
    let server_log = phux_config::instance::server_log_path();
    assert_eq!(
        lines(&AttachError::Disconnected),
        [
            "the server closed the connection unexpectedly".to_owned(),
            format!("server log: {}", server_log.display()),
            DOCTOR.to_owned(),
        ]
    );
    let version = phux_protocol::PROTOCOL_VERSION;
    assert_eq!(
        lines(&AttachError::Protocol("bad frame".to_owned())),
        [
            "attach failed: protocol error: bad frame".to_owned(),
            "client log: /state/phux/client-42.log".to_owned(),
            format!(
                "{DOCTOR} (client protocol {}.{}.{})",
                version.major, version.minor, version.patch,
            ),
        ]
    );
}

/// A live old server prints the stop-and-start recovery, with the pid when
/// it is known, and never the generic doctor / upgrade / stale / remove advice.
#[test]
fn incompatible_lines_are_the_stop_recovery_with_and_without_a_pid() {
    let socket = Path::new("/tmp/old.sock");
    let log = Path::new("/state/phux/server.log");
    let err = AttachError::IncompatibleProtocol(
        "incompatible protocol: client offered 1.4.0, server requires 0.9.x; \
         update the phux server so protocol major.minor match"
            .to_owned(),
    );
    for pid in [Some(4242), None] {
        let lines = no_server_lines(&err, socket, "ls", log, pid);
        assert_eq!(
            lines,
            crate::commands::diagnostics::old_server_recovery_lines(socket, pid)
        );
        assert_eq!(
            attach_error_lines(&err, socket, "main", log, pid),
            lines,
            "attach and connect print the same recovery"
        );
        let text = lines.join("\n");
        assert_stop_recovery(&text, pid);
        assert!(text.contains("`lsof -nP -- '/tmp/old.sock'`"), "{text}");
    }
}

/// A scripted server that refuses `HELLO` the way v0.110 (`0.9.x`) does.
/// The client classifies it by `VERSION_INCOMPATIBLE`, and every hint names
/// `kill <pid>` plus `phux server --ensure` without calling the live socket stale.
#[cfg(unix)]
#[test]
fn a_v0_110_hello_refusal_prints_the_stop_and_start_recovery() {
    use std::process::ExitCode;

    let version = phux_protocol::PROTOCOL_VERSION;
    let message = format!(
        "incompatible protocol: client offered {}.{}.{}, server requires 0.9.x; \
         update the phux server so protocol major.minor match",
        version.major, version.minor, version.patch,
    );
    let dir = tempfile::tempdir().expect("temp dir");
    let (socket, _seen) = old_protocol_server(dir.path(), message.clone());

    let pid = crate::commands::diagnostics::server_pid(&socket)
        .expect("peer credentials name the scripted server");
    let ours = i32::try_from(std::process::id()).expect("pid fits in i32");
    assert_eq!(
        pid, ours,
        "the in-process scripted server's pid is this test"
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let state = runtime.block_on(phux_client::state::get_state(&socket));
    let err = state.expect_err("HELLO was refused");
    assert!(
        matches!(&err, AttachError::IncompatibleProtocol(text) if text == &message),
        "{err:?}"
    );

    let upgrade = crate::commands::upgrade::request_upgrade(&socket);
    assert!(
        matches!(&upgrade, Err(AttachError::IncompatibleProtocol(text)) if text == &message),
        "{upgrade:?}"
    );

    let code = crate::commands::diagnostics::report_no_server(&err, &socket, "upgrade");
    assert_eq!(code, ExitCode::FAILURE);

    let log = Path::new("/state/phux/server.log");
    for text in [
        no_server_lines(&err, &socket, "upgrade", log, Some(pid)).join("\n"),
        attach_error_lines(&err, &socket, "main", log, Some(pid)).join("\n"),
    ] {
        assert_stop_recovery(&text, Some(pid));
    }

    let check = super::doctor::check_server(&socket);
    assert_eq!(check.status, super::doctor::Status::Fail);
    let doctor = format!("{}\n{}", check.detail, check.hint.unwrap_or_default());
    assert_stop_recovery(&doctor, Some(pid));
}

#[cfg(unix)]
fn old_protocol_server(
    dir: &Path,
    message: String,
) -> (
    std::path::PathBuf,
    std::sync::mpsc::Receiver<Vec<phux_protocol::wire::frame::FrameKind>>,
) {
    use phux_protocol::wire::frame::ErrorCode;

    let socket = dir.join("phux.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let (record, seen) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("script runtime");
        runtime.block_on(async move {
            let listener = tokio::net::UnixListener::from_std(listener).expect("tokio listener");
            loop {
                let spec = phux_client::testkit::ScriptSpec::new()
                    .refuse_hello(ErrorCode::VersionIncompatible, &message);
                let frames = phux_client::testkit::ScriptedServer::accept(&listener, spec).await;
                if record.send(frames).is_err() {
                    return;
                }
            }
        });
    });
    (socket, seen)
}

/// The stop-and-start recovery: `kill PID` and `ps` when the pid is known,
/// otherwise `lsof`, and never the stale / upgrade / remove advice.
fn assert_stop_recovery(text: &str, pid: Option<i32>) {
    if let Some(pid) = pid {
        assert!(text.contains(&format!("`kill {pid}`")), "{text}");
        let ps = format!("`ps -p {pid} -o pid=,ppid=,user=,args=`");
        assert!(text.contains(&ps), "{text}");
    } else {
        assert!(!text.contains("`kill "), "{text}");
    }
    assert!(text.contains("phux server --ensure --socket '"), "{text}");
    assert!(text.contains("closes only the panes"), "{text}");
    for forbidden in [
        "stale",
        "phux upgrade",
        "survive",
        "remove",
        "run `phux doctor`",
        "pkill",
        "killall",
    ] {
        assert!(!text.contains(forbidden), "{forbidden} in {text}");
    }
}

/// The grammar itself is tested in `phux_client::selector`; the CLI
/// door adds only the absent-target default.
#[test]
fn parse_selector_defaults_to_the_focused_session() {
    assert_eq!(parse_selector(None).unwrap(), Selector::Current);
    assert!(parse_selector(Some("")).is_err());
}
