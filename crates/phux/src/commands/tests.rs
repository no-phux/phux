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
fn agent_resolve_refusals_share_one_code_vocabulary() {
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
        AgentResolveError::KindConstant {
            name: "a".into(),
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
            super::targets::agent_resolve_code(&err),
            err.code(),
            "{err:?}"
        );
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

/// phux-i0e8.7.3: the no-server arm must name the exact start commands,
/// and every arm must end with the server log and the doctor pointer —
/// an error that does not name its remedy fails the `print_attach_error`
/// bar this helper used to miss.
#[test]
fn no_server_lines_name_start_commands_log_and_doctor() {
    let socket = Path::new("/tmp/phux-test.sock");
    let log = Path::new("/state/phux/server.log");

    let lines = no_server_lines(&refused_io(), socket, "ls", log, None);
    assert_eq!(lines[0], "no server running at /tmp/phux-test.sock");
    assert_eq!(
        lines[1],
        "start one with `phux` (attaches, auto-starting a server) or `phux server --ensure`"
    );
    assert_eq!(lines[2], "server log: /state/phux/server.log");
    assert_eq!(lines[3], "run `phux doctor` for a health check");
    assert_eq!(lines.len(), 4);
}

/// A socket that exists but cannot be dialed names the path and the OS
/// reason, not the client's internal "attach loop io error" wording.
#[test]
fn no_server_unreachable_socket_names_the_path_and_reason() {
    let denied = AttachError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    let lines = no_server_lines(
        &denied,
        Path::new("/tmp/s.sock"),
        "ls",
        Path::new("/state/phux/server.log"),
        None,
    );
    assert!(
        lines[0].starts_with("ls: cannot reach the server socket /tmp/s.sock: "),
        "{lines:?}"
    );
    assert!(!lines[0].contains("attach loop"), "{lines:?}");
    assert_eq!(lines[2], "run `phux doctor` for a health check");

    let json = super::json_err::no_server_error(&denied, Path::new("/tmp/s.sock"), "ls");
    assert_eq!(json.code, super::json_err::codes::TRANSPORT);
    assert!(json.message.contains("/tmp/s.sock"), "{}", json.message);
}

/// A mid-command disconnect is not "no server": the server was there and
/// went away, so there is no start command to suggest — but the reason it
/// went away lives in its log, so that and doctor still close the arm.
#[test]
fn no_server_disconnect_arm_names_log_and_doctor() {
    let lines = no_server_lines(
        &AttachError::Disconnected,
        Path::new("/tmp/s.sock"),
        "kill",
        Path::new("/state/phux/server.log"),
        None,
    );
    assert_eq!(lines[0], "server closed the connection during kill");
    assert_eq!(lines[1], "server log: /state/phux/server.log");
    assert_eq!(lines[2], "run `phux doctor` for a health check");
}

/// The generic arm keeps the error's own Display sentence first and still
/// ends with the remedy block.
#[test]
fn no_server_fallthrough_keeps_the_error_and_adds_remedies() {
    let lines = no_server_lines(
        &AttachError::Refused("policy said no".to_owned()),
        Path::new("/tmp/s.sock"),
        "tag",
        Path::new("/log/server.log"),
        None,
    );
    assert_eq!(
        lines[0],
        "tag failed: server refused the request: policy said no"
    );
    assert_eq!(lines[1], "server log: /log/server.log");
    assert_eq!(lines[2], "run `phux doctor` for a health check");
}

/// The three self-explaining attach arms stay single-line and keep their
/// established sentences (`phux-roz`: the no-server one is copy-pasteable).
#[test]
fn attach_error_named_arms_stay_single_line() {
    let socket = Path::new("/tmp/a.sock");
    let log = Path::new("/state/phux/client-42.log");

    assert_eq!(
        attach_error_lines(&refused_io(), socket, "main", log, None),
        ["no server at /tmp/a.sock. Start one with: phux server --ensure"]
    );
    assert_eq!(
        attach_error_lines(
            &AttachError::Refused("no such session".to_owned()),
            socket,
            "main",
            log,
            None,
        ),
        ["server refused attach: no such session"]
    );
    assert_eq!(
        attach_error_lines(&AttachError::NotATty, socket, "main", log, None),
        ["attach requires an interactive terminal (stdin is not a TTY)."]
    );
}

/// phux-i0e8.2.3: a mid-session disconnect that reaches the printer
/// without the reconnect window (e.g. `phux new`'s attach tail) names
/// its cause, the SERVER log (the reason the server went away lives
/// there, not in this client's log), and the doctor remedy.
#[test]
fn attach_error_disconnected_arm_names_the_remedy() {
    let lines = attach_error_lines(
        &AttachError::Disconnected,
        Path::new("/tmp/a.sock"),
        "main",
        Path::new("/state/phux/client-42.log"),
        None,
    );
    assert_eq!(lines[0], "the server closed the connection unexpectedly");
    assert_eq!(
        lines[1],
        format!(
            "server log: {}",
            phux_server::telemetry::server_log_path().display()
        )
    );
    assert_eq!(lines[2], "run `phux doctor` for a health check");
    assert_eq!(lines.len(), 3);
}

/// phux-i0e8.7.3: the fallthrough (`Protocol`/`Terminal`/…)
/// used to end at "attach failed: {err}" with nowhere to go. It must now
/// name this client's own log, doctor, and the client protocol triple —
/// a `Protocol` error usually means the binaries disagree, and doctor
/// prints both sides.
#[test]
fn attach_error_fallthrough_names_client_log_doctor_and_triple() {
    let lines = attach_error_lines(
        &AttachError::Protocol("bad frame".to_owned()),
        Path::new("/tmp/a.sock"),
        "main",
        Path::new("/state/phux/client-42.log"),
        None,
    );
    assert_eq!(lines[0], "attach failed: protocol error: bad frame");
    assert_eq!(lines[1], "client log: /state/phux/client-42.log");
    let version = phux_protocol::PROTOCOL_VERSION;
    assert_eq!(
        lines[2],
        format!(
            "run `phux doctor` for a health check (client protocol {}.{}.{})",
            version.major, version.minor, version.patch,
        )
    );
    assert_eq!(lines.len(), 3);
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
        if let Some(pid) = pid {
            assert!(text.contains(&format!("`kill {pid}`")), "{text}");
        } else {
            assert!(!text.contains("`kill "), "{text}");
        }
        assert!(text.contains("`lsof -nP -- '/tmp/old.sock'`"), "{text}");
        assert!(
            text.contains("phux server --ensure --socket '/tmp/old.sock'`"),
            "{text}"
        );
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
        assert_live_old_server_hint(&text, pid);
    }

    let check = super::doctor::check_server(&socket);
    assert_eq!(check.status, super::doctor::Status::Fail);
    let doctor = format!("{}\n{}", check.detail, check.hint.unwrap_or_default());
    assert_live_old_server_hint(&doctor, pid);
    assert!(
        !doctor.contains("remove it and start a fresh server"),
        "{doctor}"
    );
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

fn assert_live_old_server_hint(text: &str, pid: i32) {
    assert!(text.contains(&format!("`kill {pid}`")), "{text}");
    assert!(
        text.contains(&format!("`ps -p {pid} -o pid=,ppid=,user=,args=`")),
        "{text}"
    );
    assert!(text.contains("phux server --ensure --socket '"), "{text}");
    assert!(text.contains("closes only the panes"), "{text}");
    for forbidden in [
        "stale",
        "phux upgrade",
        "survive",
        "remove",
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
