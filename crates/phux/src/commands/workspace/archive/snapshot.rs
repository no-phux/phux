use std::collections::{BTreeMap, HashMap};

use crate::commands::agent::AgentSessionRecord;
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::info::{LayoutNode, ResourceInfo, SessionSnapshot, SplitDir, WindowInfo};

use super::model::{
    ARCHIVE_SCHEMA_VERSION, WorkspaceAgentSession, WorkspaceArchive, WorkspaceLayoutNode,
    WorkspacePane, WorkspaceSession, WorkspaceSplitDir, WorkspaceWindow,
};

pub(super) fn archive_from_snapshot(
    snapshot: &SessionSnapshot,
    agent_sessions: &HashMap<ResourceId, AgentSessionRecord>,
) -> WorkspaceArchive {
    let windows_by_session = windows_by_session(&snapshot.windows);
    let panes_by_window = panes_by_window(&snapshot.resources);
    let sessions = snapshot
        .sessions
        .iter()
        .map(|session| {
            let windows = windows_by_session
                .get(&session.id)
                .into_iter()
                .flat_map(|windows| windows.iter())
                .map(|window| {
                    archive_window(
                        window,
                        session.active_window,
                        &panes_by_window,
                        agent_sessions,
                    )
                })
                .collect();
            WorkspaceSession {
                name: session.name.clone(),
                active: session.id == snapshot.focused_session,
                cwd: None,
                command: None,
                windows,
            }
        })
        .collect();
    WorkspaceArchive {
        schema_version: ARCHIVE_SCHEMA_VERSION,
        sessions,
    }
}

fn archive_window(
    window: &WindowInfo,
    active_window: Option<WindowId>,
    panes_by_window: &BTreeMap<WindowId, Vec<&ResourceInfo>>,
    agent_sessions: &HashMap<ResourceId, AgentSessionRecord>,
) -> WorkspaceWindow {
    let panes = panes_by_window.get(&window.id).cloned().unwrap_or_default();
    let pane_index = panes
        .iter()
        .enumerate()
        .map(|(index, pane)| (pane.id.clone(), index))
        .collect();
    WorkspaceWindow {
        name: window.name.clone(),
        active: Some(window.id) == active_window,
        layout: window
            .layout
            .as_ref()
            .and_then(|layout| archive_layout(layout, &pane_index)),
        panes: panes
            .into_iter()
            .map(|pane| WorkspacePane {
                active: Some(pane.id.clone()) == window.active_resource,
                title: pane.title.clone(),
                cwd: pane.cwd.clone(),
                command: None,
                agent_session: agent_sessions
                    .get(&pane.id)
                    .map(|record| WorkspaceAgentSession {
                        plugin_id: record.plugin_id.clone(),
                        integration_id: record.integration_id.clone(),
                        native_id: record.native_id.clone(),
                    }),
                cols: pane.cols,
                rows: pane.rows,
            })
            .collect(),
    }
}

fn windows_by_session(windows: &[WindowInfo]) -> BTreeMap<SessionId, Vec<&WindowInfo>> {
    let mut grouped: BTreeMap<SessionId, Vec<&WindowInfo>> = BTreeMap::new();
    for window in windows {
        grouped.entry(window.session_id).or_default().push(window);
    }
    for entries in grouped.values_mut() {
        entries.sort_by_key(|window| window.index);
    }
    grouped
}

fn panes_by_window(panes: &[ResourceInfo]) -> BTreeMap<WindowId, Vec<&ResourceInfo>> {
    let mut grouped: BTreeMap<WindowId, Vec<&ResourceInfo>> = BTreeMap::new();
    for pane in panes {
        grouped.entry(pane.window_id).or_default().push(pane);
    }
    grouped
}

fn archive_layout(
    layout: &LayoutNode,
    pane_index: &BTreeMap<ResourceId, usize>,
) -> Option<WorkspaceLayoutNode> {
    match layout {
        LayoutNode::Leaf(id) => pane_index
            .get(id)
            .copied()
            .map(|pane| WorkspaceLayoutNode::Pane { pane }),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => Some(WorkspaceLayoutNode::Split {
            dir: split_dir(*dir)?,
            ratio: *ratio,
            left: Box::new(archive_layout(left, pane_index)?),
            right: Box::new(archive_layout(right, pane_index)?),
        }),
        _ => None,
    }
}

const fn split_dir(dir: SplitDir) -> Option<WorkspaceSplitDir> {
    match dir {
        SplitDir::Horizontal => Some(WorkspaceSplitDir::Horizontal),
        SplitDir::Vertical => Some(WorkspaceSplitDir::Vertical),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use phux_protocol::ids::{ResourceId, SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

    use super::*;

    #[test]
    fn projects_snapshot_into_workspace_archive() {
        let session = SessionInfo::new(SessionId::new(1), "ops")
            .with_active_window(Some(WindowId::new(2)))
            .with_window_count(1);
        let window = WindowInfo::new(WindowId::new(2), SessionId::new(1), "main")
            .with_active_resource(Some(ResourceId::local(3)));
        let pane = ResourceInfo::new(ResourceId::local(3), WindowId::new(2), 120, 40)
            .with_title(Some("monitor".to_owned()))
            .with_cwd(Some("/tmp/phux-ops".to_owned()));
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(2), ResourceId::local(3))
                .with_sessions(vec![session])
                .with_windows(vec![window])
                .with_resources(vec![pane]);
        let agent_sessions = HashMap::from([(
            ResourceId::local(3),
            AgentSessionRecord::new("com.phux.agents", "claude-code", "session-42")
                .expect("valid record"),
        )]);

        let archive = archive_from_snapshot(&snapshot, &agent_sessions);

        assert_eq!(archive.schema_version, ARCHIVE_SCHEMA_VERSION);
        assert_eq!(archive.sessions[0].name, "ops");
        assert!(archive.sessions[0].active);
        assert_eq!(
            archive.sessions[0].windows[0].panes[0].title.as_deref(),
            Some("monitor")
        );
        assert_eq!(
            archive.sessions[0].windows[0].panes[0].cwd.as_deref(),
            Some("/tmp/phux-ops")
        );
        assert_eq!(archive.sessions[0].windows[0].panes[0].command, None);
        assert_eq!(
            archive.sessions[0].windows[0].panes[0]
                .agent_session
                .as_ref()
                .map(|record| record.native_id.as_str()),
            Some("session-42")
        );
    }

    #[test]
    fn marks_only_session_active_window_as_active() {
        let session = SessionInfo::new(SessionId::new(1), "ops")
            .with_active_window(Some(WindowId::new(3)))
            .with_window_count(2);
        let inactive_window = WindowInfo::new(WindowId::new(2), SessionId::new(1), "left")
            .with_active_resource(Some(ResourceId::local(4)));
        let active_window = WindowInfo::new(WindowId::new(3), SessionId::new(1), "right")
            .with_index(1)
            .with_active_resource(Some(ResourceId::local(5)));
        let panes = vec![
            ResourceInfo::new(ResourceId::local(4), WindowId::new(2), 80, 24),
            ResourceInfo::new(ResourceId::local(5), WindowId::new(3), 80, 24),
        ];
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(3), ResourceId::local(5))
                .with_sessions(vec![session])
                .with_windows(vec![inactive_window, active_window])
                .with_resources(panes);

        let archive = archive_from_snapshot(&snapshot, &HashMap::new());

        assert!(!archive.sessions[0].windows[0].active);
        assert!(archive.sessions[0].windows[1].active);
    }
}
