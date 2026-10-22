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

    let lines = no_server_lines(&refused_io(), socket, "ls", log);
    assert_eq!(lines[0], "no server running at /tmp/phux-test.sock");
    assert_eq!(
        lines[1],
        "start one with `phux` (attaches, auto-starting a server) or `phux server`"
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
    );
    assert_eq!(
        lines[0],
        "tag failed: server refused attach: policy said no"
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
        attach_error_lines(&refused_io(), socket, "main", log),
        ["no server at /tmp/a.sock. Start one with: phux server --session main"]
    );
    assert_eq!(
        attach_error_lines(
            &AttachError::Refused("no such session".to_owned()),
            socket,
            "main",
            log,
        ),
        ["server refused attach: no such session"]
    );
    assert_eq!(
        attach_error_lines(&AttachError::NotATty, socket, "main", log),
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

/// The grammar itself is tested in `phux_client::selector`; the CLI
/// door adds only the absent-target default.
#[test]
fn parse_selector_defaults_to_the_focused_session() {
    assert_eq!(parse_selector(None).unwrap(), Selector::Current);
    assert!(parse_selector(Some("")).is_err());
}
