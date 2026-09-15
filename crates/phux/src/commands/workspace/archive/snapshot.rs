use std::collections::{BTreeMap, HashMap, HashSet};

use crate::commands::agent::AgentSessionRecord;
use phux_client::layout::{LayoutState, WindowState, Workspace, kill_pane, leaves};
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::info::{
    LayoutNode, ResourceInfo, SessionInfo, SessionSnapshot, SplitDir, WindowInfo,
};

use super::model::{
    ARCHIVE_SCHEMA_VERSION, WorkspaceAgentSession, WorkspaceArchive, WorkspaceLayoutNode,
    WorkspacePane, WorkspaceSession, WorkspaceSplitDir, WorkspaceWindow,
};

/// Build the JSON-ready archive from a `GET_STATE` snapshot, each session's
/// decoded L3 layout envelope when one was found (`layouts`, ADR-0129
/// review item 9), and the resolved native agent records.
///
/// `GET_STATE`'s own `WindowInfo.layout` is never populated by the
/// reference server, so `layouts` — read separately, over `SET_METADATA`'s
/// sibling `GET_METADATA`, by the save driver — is the only source that can
/// carry a session's real split tree; a session absent from it (never
/// attached, or an undecodable value) falls back to the bare
/// one-window-per-registry-window projection every session used before
/// this lane.
pub(super) fn archive_from_snapshot(
    snapshot: &SessionSnapshot,
    agent_sessions: &HashMap<ResourceId, AgentSessionRecord>,
    layouts: &HashMap<SessionId, Workspace>,
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
            WorkspaceSession {
                name: session.name.clone(),
                active: session.id == snapshot.focused_session,
                cwd: None,
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

/// One session's archived windows: from its decoded L3 layout envelope
/// when `layout` names one (ADR-0129 review item 9), else the bare
/// one-window-per-registry-window projection every session used before
/// this lane.
///
/// A stored layout is reconciled against the session's actual live panes
/// first (review items 1+2): [`reconcile_layout`] drops any leaf that is
/// no longer live and collapses its parent split, and any live pane
/// present in no window at all — a headless `phux spawn` never touches
/// L3 layout, so this is the normal shape for an unplaced pane, not an
/// edge case — is placed in one synthesized `"unplaced"` window, with a
/// line pushed onto `warnings` naming it.
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

/// Drop every layout leaf that is not in `live_ids` (a closed pane's leaf
/// otherwise lingers in a stored layout forever — the server never prunes
/// it), collapsing each affected split so the sibling takes its place; a
/// window whose every leaf dies this way is dropped entirely.
///
/// Returns the surviving windows, each paired with whether it was
/// `layout`'s own active window, plus — in `live_ids`'s own (registry)
/// order — every live pane that ended up in no surviving window, whether
/// because it was never in the stored layout at all or because its window
/// died out from under it.
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

/// Archive one window straight from its decoded L3 layout state: the pane
/// order is the tree's own leaves in left-to-right DFS order (matching
/// `phux-client-core`'s own `leaves()` walk, the same order `restore`
/// expects a `WorkspaceLayoutNode::Pane` index to name), with `cwd`/`title`
/// and any resolved agent session looked up per leaf from the snapshot's
/// `ResourceInfo` list.
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

        let (archive, warnings) =
            archive_from_snapshot(&snapshot, &agent_sessions, &HashMap::new());
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
            archive_from_snapshot(&snapshot, &HashMap::new(), &HashMap::new());
        assert!(warnings.is_empty(), "{warnings:?}");

        assert!(!archive.sessions[0].windows[0].active);
        assert!(archive.sessions[0].windows[1].active);
    }
}
