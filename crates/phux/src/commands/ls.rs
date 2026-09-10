use std::process::ExitCode;

use phux_client::resource;
use phux_client::state::{Degradation, StateView};
use phux_core::session_list::{ResourceJson, SessionJson, SessionListJson};

use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};

use crate::commands::partial;
use crate::commands::server_target::ServerSpec;

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
///
/// `server` is the local socket or a `--remote` host; the listing is the
/// same either way (see `server_target`).
pub(crate) fn run_ls(json: bool, server: ServerSpec) -> ExitCode {
    let (rt, target) = match server.prepare("ls", json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    match rt.block_on(target.get_state()) {
        Ok(view) => render_listing(json, view),
        Err(err) => target.report_unreachable(json, &err, "ls"),
    }
}

/// Print one fetched listing in the requested shape.
fn render_listing(json: bool, view: StateView) -> ExitCode {
    let (snapshot, degradation) = view.into_parts();
    if json {
        // Not stderr: a `--json` consumer's channel is the document.
        return print_sessions_json(&snapshot, &degradation);
    }
    print_sessions(&snapshot);
    partial::warn_partial_view("ls", &degradation);
    ExitCode::SUCCESS
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

/// The human `phux ls` body as pure data: name-sorted session lines, each
/// followed by the panes of that session that carry agent sessions (the
/// sessions nested under their pane), then satellite Terminals. Empty
/// exactly when the server has nothing to list — the trigger for
/// [`EMPTY_STATE`]. Split from [`print_sessions`] so the rendering is
/// unit-testable without capturing stdout.
///
/// A pane with no agent session prints nothing under its session line, so a
/// server that serves only Terminals renders exactly the pre-resource-model
/// listing.
fn session_lines(snapshot: &SessionSnapshot) -> Vec<String> {
    let mut sessions: Vec<_> = snapshot.sessions.iter().collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    let mut lines: Vec<String> = Vec::new();
    for session in sessions {
        lines.push(format_session_line(session));
        lines.extend(agent_session_lines(snapshot, session));
    }
    for pane in resource::terminals(snapshot) {
        if pane.id.host().is_some() {
            lines.push(format!(
                "{}: satellite terminal",
                crate::selector::format_terminal_id(&pane.id)
            ));
        }
    }
    lines
}

/// The nested lines under one session: `  @7` for each of its panes that
/// hosts an agent session, then `    @9: agent session <provider> (<state>)`
/// per session child.
fn agent_session_lines(snapshot: &SessionSnapshot, session: &SessionInfo) -> Vec<String> {
    let mut lines = Vec::new();
    for window in snapshot
        .windows
        .iter()
        .filter(|window| window.session_id == session.id)
    {
        for pane in resource::terminals(snapshot).filter(|pane| pane.window_id == window.id) {
            let children: Vec<_> = resource::children_of(snapshot, &pane.id).collect();
            if children.is_empty() {
                continue;
            }
            lines.push(format!(
                "  {}",
                crate::selector::format_terminal_id(&pane.id)
            ));
            for child in children {
                let facet = child.agent.as_ref();
                let provider = facet.map_or("agent session", |facet| facet.provider.as_str());
                let state = facet.map_or("unknown", |facet| facet.state.as_str());
                lines.push(format!(
                    "    {}: agent session {provider} ({state})",
                    crate::selector::format_terminal_id(&child.id)
                ));
            }
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
    // `terminals` stays the Terminal-kind inventory — the ids a Terminal-facet
    // verb accepts — so a consumer that iterates it and calls `snapshot` on
    // each keeps working; every resource, with its kind and parent, is the
    // additive `resources` array.
    let terminals = resource::terminals(snapshot)
        .map(|pane| crate::selector::format_terminal_id(&pane.id))
        .collect();
    let resources = snapshot
        .resources
        .iter()
        .map(|pane| ResourceJson {
            id: crate::selector::format_terminal_id(&pane.id),
            kind: resource::kind_name(pane.kind).to_owned(),
            parent: pane
                .parent
                .as_ref()
                .map(crate::selector::format_terminal_id),
        })
        .collect();
    let list = SessionListJson::new(entries)
        .with_terminals(terminals)
        .with_resources(resources)
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
    use phux_protocol::{ResourceId, SessionId, WindowId};

    use super::{EMPTY_STATE, format_session_line, session_lines};

    fn session(name: &str, windows: u16, clients: u16) -> SessionInfo {
        SessionInfo::new(SessionId::new(1), name)
            .with_window_count(windows)
            .with_attached_client_count(clients)
    }

    #[test]
    fn empty_snapshot_yields_no_lines_and_the_empty_state_names_next_commands() {
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1));
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

    /// An agent session nests under its pane, under its session; panes with
    /// no session child print nothing, so the plain listing is unchanged.
    #[test]
    fn agent_sessions_nest_under_their_pane() {
        use phux_protocol::ids::ResourceKind;
        use phux_protocol::wire::info::{AgentFacet, ResourceInfo, WindowInfo};

        let work = SessionId::new(1);
        let window = WindowId::new(10);
        let snapshot = SessionSnapshot::new(work, window, ResourceId::local(7))
            .with_sessions(vec![SessionInfo::new(work, "work").with_window_count(1)])
            .with_windows(vec![WindowInfo::new(window, work, "shell").with_index(0)])
            .with_resources(vec![
                ResourceInfo::new(ResourceId::local(7), window, 80, 24),
                ResourceInfo::new(ResourceId::local(8), window, 80, 24),
                ResourceInfo::new(ResourceId::local(9), window, 0, 0)
                    .with_kind(ResourceKind::AgentSession)
                    .with_parent(Some(ResourceId::local(7)))
                    .with_agent(Some(AgentFacet::new("claude", "working"))),
            ]);
        assert_eq!(
            session_lines(&snapshot),
            vec![
                "work: 1 window",
                "  @7",
                "    @9: agent session claude (working)",
            ]
        );
    }

    /// `--json`: `terminals` lists Terminal-kind ids only; `resources` lists
    /// every resource with its kind and parent.
    #[test]
    fn json_splits_terminals_from_resources() {
        use phux_protocol::ids::ResourceKind;
        use phux_protocol::wire::info::ResourceInfo;

        let window = WindowId::new(10);
        let snapshot = SessionSnapshot::new(SessionId::new(1), window, ResourceId::local(7))
            .with_resources(vec![
                ResourceInfo::new(ResourceId::local(7), window, 80, 24),
                ResourceInfo::new(ResourceId::local(9), window, 0, 0)
                    .with_kind(ResourceKind::AgentSession)
                    .with_parent(Some(ResourceId::local(7))),
            ]);
        let terminals: Vec<String> = phux_client::resource::terminals(&snapshot)
            .map(|pane| crate::selector::format_terminal_id(&pane.id))
            .collect();
        assert_eq!(terminals, ["@7"]);
        let resources: Vec<(String, String, Option<String>)> = snapshot
            .resources
            .iter()
            .map(|pane| {
                (
                    crate::selector::format_terminal_id(&pane.id),
                    phux_client::resource::kind_name(pane.kind).to_owned(),
                    pane.parent
                        .as_ref()
                        .map(crate::selector::format_terminal_id),
                )
            })
            .collect();
        assert_eq!(
            resources,
            [
                ("@7".to_owned(), "terminal".to_owned(), None),
                (
                    "@9".to_owned(),
                    "agent_session".to_owned(),
                    Some("@7".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn lines_are_name_sorted() {
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::new(1))
                .with_sessions(vec![session("beta", 1, 0), session("alpha", 1, 0)]);
        let lines = session_lines(&snapshot);
        assert_eq!(lines, vec!["alpha: 1 window", "beta: 1 window"]);
    }
}
