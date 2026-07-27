use std::path::PathBuf;
use std::process::ExitCode;

use phux_core::session_list::{SessionJson, SessionListJson};

use phux_protocol::wire::info::SessionSnapshot;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, report_no_server};

/// `phux ls` — list sessions via `GET_STATE`. Does not auto-start a
/// server. With `json`, emits the stable [`SessionListJson`] contract
/// (ADR-0022); otherwise the human text from [`print_sessions`].
pub(crate) fn run_ls(json: bool, socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    match rt.block_on(phux_client::state::get_state(&socket_path)) {
        Ok(snapshot) => {
            if json {
                print_sessions_json(&snapshot)
            } else {
                print_sessions(&snapshot);
                ExitCode::SUCCESS
            }
        }
        Err(err) => report_no_server(&err, &socket_path, "ls"),
    }
}

/// Render the session list, one line per session (tmux-`ls`-ish), followed
/// by satellite Terminals that cannot be joined to hub-local sessions.
pub(crate) fn print_sessions(snapshot: &SessionSnapshot) {
    let mut sessions: Vec<_> = snapshot.sessions.iter().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    for s in sessions {
        let windows = if s.window_count == 1 {
            "window"
        } else {
            "windows"
        };
        let attached = if s.attached_client_count > 0 {
            " (attached)"
        } else {
            ""
        };
        outln!("{}: {} {windows}{attached}", s.name, s.window_count);
    }
    for pane in &snapshot.panes {
        if pane.id.host().is_some() {
            outln!(
                "{}: satellite terminal",
                crate::selector::format_terminal_id(&pane.id)
            );
        }
    }
}

/// Emit the session list as the stable [`SessionListJson`] contract.
///
/// Sessions are name-sorted to match [`print_sessions`], keeping the two
/// views consistent and the JSON stable across runs.
pub(crate) fn print_sessions_json(snapshot: &SessionSnapshot) -> ExitCode {
    let mut sessions: Vec<_> = snapshot.sessions.iter().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    let entries = sessions
        .into_iter()
        .map(|s| SessionJson {
            name: s.name.clone(),
            windows: s.window_count,
            attached: s.attached_client_count > 0,
        })
        .collect();
    let terminals = snapshot
        .panes
        .iter()
        .map(|pane| crate::selector::format_terminal_id(&pane.id))
        .collect();
    let list = SessionListJson::new(entries).with_terminals(terminals);
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
