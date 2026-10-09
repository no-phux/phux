use std::collections::{BTreeMap, HashMap, HashSet};

use phux_client::agent_session_record::AgentSessionRecord;
use phux_client::layout::{
    LayoutNode, LayoutState, SplitDir, WindowState, Workspace, kill_pane, leaves,
};
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

use super::model::{
    ARCHIVE_SCHEMA_VERSION, WorkspaceAgentSession, WorkspaceArchive, WorkspaceLayoutNode,
    WorkspacePane, WorkspaceSession, WorkspaceSplitDir, WorkspaceWindow,
};

/// Build the archive from a `GET_STATE` snapshot, each session's decoded L3
/// layout (`layouts`; `GET_STATE` itself never carries one), and the resolved
/// agent records. A session without a layout falls back to the
/// one-window-per-registry-window projection.
pub(super) fn archive_from_snapshot(
    snapshot: &SessionSnapshot,
    agent_sessions: &HashMap<ResourceId, AgentSessionRecord>,
    layouts: &HashMap<SessionId, Workspace>,
    projects: &HashMap<String, String>,
) -> (WorkspaceArchive, Vec<String>) {
    let windows_by_session = windows_by_session(&snapshot.windows);
    let panes_by_window = panes_by_window(&snapshot.resources);
    let resources_by_id: HashMap<ResourceId, &ResourceInfo> = snapshot
        .resources
        .iter()
        .map(|resource| (resource.id.clone(), resource))
        .collect();
    let mut warnings = Vec::new();
    let sessions = snapshot
        .sessions
        .iter()
        .map(|session| {
            let windows = archive_windows_for_session(
                session,
                layouts.get(&session.id),
                &windows_by_session,
                &panes_by_window,
                &resources_by_id,
                agent_sessions,
                &mut warnings,
            );
            let (cwd, host) = focused_place(session, snapshot);
            WorkspaceSession {
                name: session.name.clone(),
                active: session.id == snapshot.focused_session,
                cwd,
                host,
                project: projects.get(&session.name).cloned(),
                command: None,
                windows,
            }
        })
        .collect();
    (
        WorkspaceArchive {
            schema_version: ARCHIVE_SCHEMA_VERSION,
            sessions,
        },
        warnings,
    )
}

/// The focused pane's directory and satellite host. A local pane has no host.
/// When the active window has no focused pane, the first pane in the session
/// that has a directory supplies it.
fn focused_place(
    session: &SessionInfo,
    snapshot: &SessionSnapshot,
) -> (Option<String>, Option<String>) {
    let in_session = |window: &WindowInfo| window.session_id == session.id;
    let focused_id = session
        .active_window
        .and_then(|window_id| {
            snapshot
                .windows
                .iter()
                .find(|window| window.id == window_id && in_session(window))
        })
        .and_then(|window| window.active_resource.clone())
        .or_else(|| {
            snapshot.resources.iter().find_map(|resource| {
                snapshot
                    .windows
                    .iter()
                    .any(|window| in_session(window) && window.id == resource.window_id)
                    .then(|| resource.id.clone())
            })
        });
    let info = focused_id.as_ref().and_then(|id| {
        snapshot
            .resources
            .iter()
            .find(|resource| resource.id == *id)
    });
    let cwd = info.and_then(|resource| resource.cwd.clone());
    let host = focused_id
        .as_ref()
        .and_then(|id| id.host().map(|host| host.as_str().to_owned()));
    (cwd, host)
}

/// One session's archived windows, from its L3 layout when present, else
/// the registry projection. A stored layout is first reconciled with the live
/// panes ([`reconcile_layout`]); live panes placed nowhere (a headless spawn,
/// normally) go into a synthesized `"unplaced"` window, with a warning.
fn archive_windows_for_session(
    session: &SessionInfo,
    layout: Option<&Workspace>,
    windows_by_session: &BTreeMap<SessionId, Vec<&WindowInfo>>,
    panes_by_window: &BTreeMap<WindowId, Vec<&ResourceInfo>>,
    resources_by_id: &HashMap<ResourceId, &ResourceInfo>,
    agent_sessions: &HashMap<ResourceId, AgentSessionRecord>,
    warnings: &mut Vec<String>,
) -> Vec<WorkspaceWindow> {
    let session_windows = windows_by_session.get(&session.id).into_iter().flatten();
    let Some(layout) = layout else {
        return session_windows
            .map(|window| {
                archive_window(
                    window,
                    session.active_window,
                    panes_by_window,
                    agent_sessions,
                )
            })
            .collect();
    };
    let live_ids: Vec<ResourceId> = session_windows
        .flat_map(|window| panes_by_window.get(&window.id).into_iter().flatten())
        .map(|pane| pane.id.clone())
        .collect();
    let (kept, unplaced) = reconcile_layout(layout, &live_ids);
    let mut windows: Vec<WorkspaceWindow> = kept
        .into_iter()
        .map(|(active, window)| {
            archive_window_from_layout(&window, active, resources_by_id, agent_sessions)
        })
        .collect();
    if !unplaced.is_empty() {
        warnings.push(format!(
            "workspace save: session {:?} has {} live pane(s) absent from its stored layout \
             (placed in a synthesized \"unplaced\" window): {}",
            session.name,
            unplaced.len(),
            unplaced
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
        windows.push(unplaced_window(&unplaced, resources_by_id, agent_sessions));
    }
    windows
}

/// Drop every layout leaf not in `live_ids` (the server never prunes closed
/// panes from stored layouts), collapsing each affected split and dropping a
/// window that loses every leaf. Returns the surviving windows (with whether
/// each was active) and, in registry order, the live panes left unplaced.
fn reconcile_layout(
    layout: &Workspace,
    live_ids: &[ResourceId],
) -> (Vec<(bool, WindowState)>, Vec<ResourceId>) {
    let live: HashSet<ResourceId> = live_ids.iter().cloned().collect();
    let mut placed: HashSet<ResourceId> = HashSet::new();
    let mut kept = Vec::with_capacity(layout.windows.len());
    for (index, window) in layout.windows.iter().enumerate() {
        let Some(mut tree) = window.state.tree.clone() else {
            continue;
        };
        let mut survives = true;
        while let Some(dead) = leaves(&tree).into_iter().find(|id| !live.contains(id)) {
            // `Ok(None)`: `dead` was the tree's last leaf, so the whole
            // window dies. `Err`: unreachable — `dead` was just found via
            // this same tree's own `leaves()`.
            if let Ok(Some(next)) = kill_pane(&tree, &dead) {
                tree = next;
            } else {
                survives = false;
                break;
            }
        }
        if !survives {
            continue;
        }
        placed.extend(leaves(&tree));
        kept.push((
            index == layout.active,
            WindowState {
                id: window.id,
                name: window.name.clone(),
                state: LayoutState {
                    focus: window.state.focus.clone().filter(|id| live.contains(id)),
                    tree: Some(tree),
                },
            },
        ));
    }
    let unplaced = live_ids
        .iter()
        .filter(|id| !placed.contains(*id))
        .cloned()
        .collect();
    (kept, unplaced)
}

/// A synthesized window holding every live pane a reconciled layout had
/// nowhere else to put, in a simple right-leaning chain (the same shape
/// [`super::linear_chain`] gives a schema-1 restore with no captured
/// layout at all) rather than dropping any of them from the archive.
fn unplaced_window(
    ids: &[ResourceId],
    resources_by_id: &HashMap<ResourceId, &ResourceInfo>,
    agent_sessions: &HashMap<ResourceId, AgentSessionRecord>,
) -> WorkspaceWindow {
    let pane_index: BTreeMap<ResourceId, usize> = ids
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, id)| (id, index))
        .collect();
    let layout = archive_layout(&super::linear_chain(ids), &pane_index);
    WorkspaceWindow {
        name: "unplaced".to_owned(),
        active: false,
        layout,
        panes: archive_panes(ids, None, resources_by_id, agent_sessions),
    }
}

/// Archive one window from its L3 layout: panes in the tree's left-to-right
/// leaf order (what restore indexes), with cwd, title, and agent session looked
/// up per leaf.
fn archive_window_from_layout(
    window: &WindowState,
    active: bool,
    resources_by_id: &HashMap<ResourceId, &ResourceInfo>,
    agent_sessions: &HashMap<ResourceId, AgentSessionRecord>,
) -> WorkspaceWindow {
    let ordered_panes: Vec<ResourceId> = window.state.tree.as_ref().map(leaves).unwrap_or_default();
    let pane_index: BTreeMap<ResourceId, usize> = ordered_panes
        .iter()
        .enumerate()
        .map(|(index, id)| (id.clone(), index))
        .collect();
    let layout = window
        .state
        .tree
        .as_ref()
        .and_then(|tree| archive_layout(tree, &pane_index));
    let panes = archive_panes(
        &ordered_panes,
        window.state.focus.as_ref(),
        resources_by_id,
        agent_sessions,
    );
    WorkspaceWindow {
        name: window.name.clone(),
        active,
        layout,
        panes,
    }
}

/// Build `WorkspacePane` entries for `ids`, in order, looking up
/// title/cwd/size from the registry and any resolved native agent session
/// per leaf. Shared by [`archive_window_from_layout`] and
/// [`unplaced_window`].
fn archive_panes(
    ids: &[ResourceId],
    focus: Option<&ResourceId>,
    resources_by_id: &HashMap<ResourceId, &ResourceInfo>,
    agent_sessions: &HashMap<ResourceId, AgentSessionRecord>,
) -> Vec<WorkspacePane> {
    ids.iter()
        .map(|id| {
            let info = resources_by_id.get(id).copied();
            WorkspacePane {
                active: Some(id) == focus,
                title: info.and_then(|resource| resource.title.clone()),
                cwd: info.and_then(|resource| resource.cwd.clone()),
                command: None,
                env: BTreeMap::new(),
                agent_session: agent_sessions.get(id).map(|record| WorkspaceAgentSession {
                    plugin_id: record.plugin_id.clone(),
                    integration_id: record.integration_id.clone(),
                    native_id: record.native_id.clone(),
                }),
                cols: info.map_or(0, |resource| resource.cols),
                rows: info.map_or(0, |resource| resource.rows),
            }
        })
        .collect()
}

fn archive_window(
    window: &WindowInfo,
    active_window: Option<WindowId>,
    panes_by_window: &BTreeMap<WindowId, Vec<&ResourceInfo>>,
    agent_sessions: &HashMap<ResourceId, AgentSessionRecord>,
) -> WorkspaceWindow {
    let panes = panes_by_window.get(&window.id).cloned().unwrap_or_default();
    WorkspaceWindow {
        name: window.name.clone(),
        active: Some(window.id) == active_window,
        // `GET_STATE` carries no split tree (ADR-0030); only an L3 layout does.
        layout: None,
        panes: panes
            .into_iter()
            .map(|pane| WorkspacePane {
                active: Some(pane.id.clone()) == window.active_resource,
                title: pane.title.clone(),
                cwd: pane.cwd.clone(),
                command: None,
                env: BTreeMap::new(),
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
            dir: split_dir(*dir),
            ratio: *ratio,
            left: Box::new(archive_layout(left, pane_index)?),
            right: Box::new(archive_layout(right, pane_index)?),
        }),
    }
}

const fn split_dir(dir: SplitDir) -> WorkspaceSplitDir {
    match dir {
        SplitDir::Horizontal => WorkspaceSplitDir::Horizontal,
        SplitDir::Vertical => WorkspaceSplitDir::Vertical,
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

        let (archive, warnings) =
            archive_from_snapshot(&snapshot, &agent_sessions, &HashMap::new(), &HashMap::new());
        assert!(warnings.is_empty(), "{warnings:?}");

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
        assert_eq!(archive.sessions[0].cwd.as_deref(), Some("/tmp/phux-ops"));
        assert_eq!(archive.sessions[0].host, None);
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

        let (archive, warnings) =
            archive_from_snapshot(&snapshot, &HashMap::new(), &HashMap::new(), &HashMap::new());
        assert!(warnings.is_empty(), "{warnings:?}");

        assert!(!archive.sessions[0].windows[0].active);
        assert!(archive.sessions[0].windows[1].active);
    }

    #[test]
    fn saves_the_focused_panes_host_directory_and_project() {
        let session = SessionInfo::new(SessionId::new(1), "api")
            .with_active_window(Some(WindowId::new(2)))
            .with_window_count(1);
        let pane_id = ResourceId::satellite("edge", 4);
        let window = WindowInfo::new(WindowId::new(2), SessionId::new(1), "main")
            .with_active_resource(Some(pane_id.clone()));
        let pane = ResourceInfo::new(pane_id, WindowId::new(2), 80, 24)
            .with_cwd(Some("/src/api".to_owned()));
        let snapshot = SessionSnapshot::new(
            SessionId::new(1),
            WindowId::new(2),
            ResourceId::satellite("edge", 4),
        )
        .with_sessions(vec![session])
        .with_windows(vec![window])
        .with_resources(vec![pane]);
        let mut projects = HashMap::new();
        projects.insert("api".to_owned(), "phux".to_owned());

        let (archive, warnings) =
            archive_from_snapshot(&snapshot, &HashMap::new(), &HashMap::new(), &projects);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(archive.sessions[0].host.as_deref(), Some("edge"));
        assert_eq!(archive.sessions[0].cwd.as_deref(), Some("/src/api"));
        assert_eq!(archive.sessions[0].project.as_deref(), Some("phux"));
    }
}
