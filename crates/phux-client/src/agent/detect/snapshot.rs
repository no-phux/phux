//! A whole `GET_STATE` snapshot projected pane by pane.

use std::path::Path;

use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot};

use super::{AgentStateReport, PaneEvidence, PluginAgent, SessionEvidence, infer_agent_state};
use crate::agent_meta::AgentMetaState;
use crate::selector::{AgentIndex, agent_address, format_terminal_id};

/// Every Terminal in `snapshot` projected, sorted by terminal address.
///
/// Structured `phux.agent/v1` records outrank every heuristic (ADR-0040);
/// a partial record read appends its notices to `notices`, for the caller
/// to warn about on its own channel.
pub async fn classify_snapshot(
    socket_path: &Path,
    snapshot: &SessionSnapshot,
    plugins: &[PluginAgent],
    notices: &mut Vec<String>,
) -> Vec<AgentStateReport> {
    let records = crate::agent_record::fetch_index(socket_path, snapshot, notices).await;
    // What `%name` would make of each listed record (ADR-0075 point 4).
    let index = AgentIndex::complete(records);
    let mut states = Vec::with_capacity(snapshot.resources.len());
    // Terminal-kind resources only: an `AgentSession` is reported under its
    // parent's `agent_session`, never as a row of its own.
    for pane in crate::resource::terminals(snapshot) {
        let mut evidence = pane_evidence(socket_path, snapshot, pane).await;
        evidence.record = index.get(&pane.id).cloned();
        let mut state = infer_agent_state(&evidence, plugins);
        state.set_address(agent_address(&pane.id, snapshot, &index));
        states.push(state);
    }
    states.sort_by(|a, b| a.terminal.cmp(&b.terminal));
    states
}

/// What the projection reads about one pane: its screen, its foreground
/// occupant, and its place in `snapshot`. The record is left for the
/// caller, which reads every pane's at once.
pub async fn pane_evidence(
    socket_path: &Path,
    snapshot: &SessionSnapshot,
    pane: &ResourceInfo,
) -> PaneEvidence {
    let (screen, occupant) = tokio::join!(
        crate::snapshot::get_screen_scrollback(socket_path, pane.id.clone(), None, true),
        crate::agent_meta::read_pane_occupant_within(
            socket_path,
            &pane.id,
            std::time::Duration::ZERO
        ),
    );
    let screen = screen.ok();
    let session = session_of_pane(snapshot, pane);
    PaneEvidence {
        terminal: format_terminal_id(&pane.id),
        session: session.map_or_else(|| "unknown".to_owned(), |s| s.name.clone()),
        window: window_label(snapshot, pane, session.is_some()),
        // The live OSC title from `GET_SCREEN`, the one the server's detector
        // evaluates. `pane.title` is the user-set title and is not it.
        title: screen.as_ref().and_then(|s| s.title.clone()),
        cwd: pane.cwd.clone(),
        record: None,
        agent_session: sole_agent_session(snapshot, pane),
        lines: screen.as_ref().map_or_else(Vec::new, |s| s.lines.clone()),
        semantic_input: screen
            .as_ref()
            .and_then(|s| s.cells.as_ref())
            .is_some_and(|cells| {
                cells
                    .iter()
                    .any(|cell| cell.semantic == Some(phux_core::screen::SemanticContent::Input))
            }),
        occupant,
    }
}

/// The pane's live agent session, only when it has exactly one (ADR-0103).
fn sole_agent_session(snapshot: &SessionSnapshot, pane: &ResourceInfo) -> Option<SessionEvidence> {
    let mut children = crate::resource::children_of(snapshot, &pane.id);
    let (Some(child), None) = (children.next(), children.next()) else {
        return None;
    };
    child.agent.as_ref().map(|facet| SessionEvidence {
        resource: format_terminal_id(&child.id),
        provider: facet.provider.clone(),
        native_id: facet.native_id.clone(),
        state: AgentMetaState::from(facet.state.clone()),
    })
}

/// The session `pane` belongs to, through its `session` facet
/// ([`crate::resource::session_of`]).
fn session_of_pane<'a>(
    snapshot: &'a SessionSnapshot,
    pane: &ResourceInfo,
) -> Option<&'a SessionInfo> {
    let id = crate::resource::session_of(snapshot, &pane.id)?;
    snapshot.sessions.iter().find(|session| session.id == id)
}

/// The `window` field of `phux agent --json`, kept for output stability.
/// A session is one window (ADR-0160), labelled `window-0` as the legacy
/// snapshot labels it, so a pane in a known session needs no window lookup.
/// Only a pane from a server that does not report its `session` facet is
/// labelled by its legacy window's index.
fn window_label(snapshot: &SessionSnapshot, pane: &ResourceInfo, in_session: bool) -> String {
    if pane.session.is_some() {
        return if in_session { "window-0" } else { "unknown" }.to_owned();
    }
    snapshot
        .windows
        .iter()
        .find(|window| window.id == pane.window_id)
        .map_or_else(|| "unknown".to_owned(), |w| format!("window-{}", w.index))
}

/// The `agent list --json` / `agent show --json` document. Its shape is
/// documented in docs/building.md and checked against it by a test.
#[must_use]
pub fn agents_document(states: &[AgentStateReport]) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "agents": states,
    })
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::{ResourceId, SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

    use super::{session_of_pane, window_label};

    fn snapshot(pane: &ResourceInfo) -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), pane.window_id, pane.id.clone())
            .with_sessions(vec![SessionInfo::new(SessionId::new(1), "work")])
            .with_resources(vec![pane.clone()])
    }

    /// A pane joined to its session by the `session` facet alone (no
    /// legacy window) still reports its session and the documented
    /// `window-0` label.
    #[test]
    fn a_facet_pane_names_its_session_without_a_window() {
        let pane = ResourceInfo::new(ResourceId::local(3), WindowId::new(0), 80, 24)
            .with_session(Some(SessionId::new(1)));
        let snapshot = snapshot(&pane);

        let session = session_of_pane(&snapshot, &pane);

        assert_eq!(session.map(|s| s.name.as_str()), Some("work"));
        assert_eq!(
            window_label(&snapshot, &pane, session.is_some()),
            "window-0"
        );
    }

    /// A satellite pane's facet names a session on the satellite, never one
    /// of these: both fields stay `unknown`.
    #[test]
    fn a_satellite_pane_is_unknown() {
        let pane = ResourceInfo::new(ResourceId::satellite("edge", 3), WindowId::new(0), 80, 24)
            .with_session(Some(SessionId::new(1)));
        let snapshot = snapshot(&pane);

        assert!(session_of_pane(&snapshot, &pane).is_none());
        assert_eq!(window_label(&snapshot, &pane, false), "unknown");
    }

    /// A server that predates the facet: the legacy window names both.
    #[test]
    fn a_pre_facet_pane_falls_back_to_its_window() {
        let pane = ResourceInfo::new(ResourceId::local(3), WindowId::new(2), 80, 24);
        let snapshot = snapshot(&pane).with_windows(vec![
            WindowInfo::new(WindowId::new(2), SessionId::new(1), "w").with_index(1),
        ]);

        let session = session_of_pane(&snapshot, &pane);

        assert_eq!(session.map(|s| s.name.as_str()), Some("work"));
        assert_eq!(window_label(&snapshot, &pane, true), "window-1");
    }
}
