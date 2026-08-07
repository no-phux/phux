use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::state::Degradation;
use phux_core::session_list::{SessionJson, SessionListJson};

use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, partial};

/// `phux ls` — list sessions via `GET_STATE`. Does not auto-start a
/// server. With `json`, emits the stable [`SessionListJson`] contract
/// (ADR-0022); otherwise the human text from [`print_sessions`].
///
/// **A partial listing still succeeds.** A federation hub that could not
/// reach a satellite answers with everything else (ADR-0007; see
/// [`partial`]), and an enumeration is true about every row it contains, so
/// the exit status stays 0 and the incompleteness is reported alongside: on
/// stderr for a human, in the payload's `unreachable` list for `--json`.
/// Making a dead satellite fail the listing would take the panes on this
/// laptop down with it.
pub(crate) fn run_ls(json: bool, socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    match rt.block_on(phux_client::state::get_state(&socket_path)) {
        Ok(view) => {
            let (snapshot, degradation) = view.into_parts();
            if json {
                // Not stderr: a `--json` consumer's channel is the document.
                print_sessions_json(&snapshot, &degradation)
            } else {
                print_sessions(&snapshot);
                partial::warn_partial_view("ls", &degradation);
                ExitCode::SUCCESS
            }
        }
        Err(err) => json_err::report_no_server(json, &err, &socket_path, "ls"),
    }
}

/// The empty listing, in the `phux host ls` mold: say what is missing,
/// then name the exact next commands. Stdout (it answers the question asked)
/// and exit 0 (an empty enumeration is a true answer, not a failure — the
/// non-zero case is *no server*, which never reaches this).
const EMPTY_STATE: [&str; 2] = [
    "No sessions.",
    "Start one with `phux` (attaches, auto-starting a server), or `phux new NAME -- CMD`.",
];

/// Render the session list, one line per session (tmux-`ls`-ish), followed
/// by satellite Terminals that cannot be joined to hub-local sessions. An
/// empty server prints [`EMPTY_STATE`] instead of nothing.
pub(crate) fn print_sessions(snapshot: &SessionSnapshot) {
    let lines = session_lines(snapshot);
    if lines.is_empty() {
        for line in EMPTY_STATE {
            outln!("{line}");
        }
        return;
    }
    for line in lines {
        outln!("{line}");
    }
}

/// The human `phux ls` body as pure data: name-sorted session lines, then
/// satellite Terminals. Empty exactly when the server has nothing to list —
/// the trigger for [`EMPTY_STATE`]. Split from [`print_sessions`] so the
/// rendering is unit-testable without capturing stdout.
fn session_lines(snapshot: &SessionSnapshot) -> Vec<String> {
    let mut sessions: Vec<_> = snapshot.sessions.iter().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    let mut lines: Vec<String> = sessions.into_iter().map(format_session_line).collect();
    for pane in &snapshot.panes {
        if pane.id.host().is_some() {
            lines.push(format!(
                "{}: satellite terminal",
                crate::selector::format_terminal_id(&pane.id)
            ));
        }
    }
    lines
}

/// One session's `ls` line, rendering the real attached-client count the
/// wire already carries (`(2 clients attached)`) rather than collapsing it
/// to a boolean `(attached)`. Zero clients says nothing.
///
/// `pub(crate)` so `phux status` renders its per-session lines through the
/// same formatter and the two views cannot drift.
pub(crate) fn format_session_line(s: &SessionInfo) -> String {
    let windows = if s.window_count == 1 {
        "window"
    } else {
        "windows"
    };
    let attached = match s.attached_client_count {
        0 => String::new(),
        1 => " (1 client attached)".to_owned(),
        n => format!(" ({n} clients attached)"),
    };
    format!("{}: {} {windows}{attached}", s.name, s.window_count)
}

/// Emit the session list as the stable [`SessionListJson`] contract.
///
/// Sessions are name-sorted to match [`print_sessions`], keeping the two
/// views consistent and the JSON stable across runs. `degradation` becomes
/// the document's `unreachable` list — always present, empty when the
/// listing is complete, so a consumer can read completeness positively
/// instead of inferring it from a missing key.
pub(crate) fn print_sessions_json(
    snapshot: &SessionSnapshot,
    degradation: &Degradation,
) -> ExitCode {
    let mut sessions: Vec<_> = snapshot.sessions.iter().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    let entries = sessions
        .into_iter()
        .map(|s| SessionJson {
            name: s.name.clone(),
            windows: s.window_count,
            attached: s.attached_client_count > 0,
            attached_clients: s.attached_client_count,
        })
        .collect();
    let terminals = snapshot
        .panes
        .iter()
        .map(|pane| crate::selector::format_terminal_id(&pane.id))
        .collect();
    let list = SessionListJson::new(entries)
        .with_terminals(terminals)
        .with_unreachable(degradation.notices().to_vec());
    match serde_json::to_string_pretty(&list) {
        Ok(s) => {
            outln!("{s}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("phux: failed to serialize session list as JSON: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
    use phux_protocol::{SessionId, TerminalId, WindowId};

    use super::{EMPTY_STATE, format_session_line, session_lines};

    fn session(name: &str, windows: u16, clients: u16) -> SessionInfo {
        SessionInfo::new(SessionId::new(1), name)
            .with_window_count(windows)
            .with_attached_client_count(clients)
    }

    #[test]
    fn empty_snapshot_yields_no_lines_and_the_empty_state_names_next_commands() {
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), TerminalId::new(1));
        assert!(session_lines(&snapshot).is_empty());

        // The two-line empty state: what is missing, then the exact commands
        // that fix it (the `phux host ls` precedent).
        assert_eq!(EMPTY_STATE.len(), 2);
        assert_eq!(EMPTY_STATE[0], "No sessions.");
        assert!(EMPTY_STATE[1].contains("`phux`"));
        assert!(EMPTY_STATE[1].contains("`phux new NAME -- CMD`"));
    }

    #[test]
    fn zero_clients_render_no_attachment_note() {
        assert_eq!(
            format_session_line(&session("work", 3, 0)),
            "work: 3 windows"
        );
    }

    #[test]
    fn one_client_renders_a_singular_count() {
        assert_eq!(
            format_session_line(&session("work", 1, 1)),
            "work: 1 window (1 client attached)"
        );
    }

    #[test]
    fn two_clients_render_the_real_count() {
        assert_eq!(
            format_session_line(&session("work", 2, 2)),
            "work: 2 windows (2 clients attached)"
        );
    }

    #[test]
    fn lines_are_name_sorted() {
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), TerminalId::new(1))
                .with_sessions(vec![session("beta", 1, 0), session("alpha", 1, 0)]);
        let lines = session_lines(&snapshot);
        assert_eq!(lines, vec!["alpha: 1 window", "beta: 1 window"]);
    }
}
