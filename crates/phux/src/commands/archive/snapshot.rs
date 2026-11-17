use std::collections::{BTreeMap, HashMap, HashSet};

use phux_client::agent_session_record::AgentSessionRecord;
use phux_client::layout::{
    ArrangementDoc, Group, LayoutNode, LayoutState, SplitDir, kill_pane, leaves,
};
use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

use super::model::{
    ARCHIVE_SCHEMA_VERSION, WorkspaceAgentSession, WorkspaceArchive, WorkspaceLayoutNode,
    WorkspacePane, WorkspaceSession, WorkspaceSplitDir, WorkspaceWindow,
};

#[derive(Clone, Copy)]
pub(super) struct AgentRecords<'a> {
    pub(super) attached: &'a HashMap<ResourceId, AgentSessionRecord>,
    pub(super) native: &'a HashMap<ResourceId, phux_protocol::native_agent::SpawnNativeAgent>,
}

/// The name a session's one archived window takes when it has no stored
/// layout: what the legacy snapshot calls a session's synthesized window.
const SESSION_WINDOW_NAME: &str = "window-0";

/// Build the archive from a `GET_STATE` snapshot, each session's decoded
/// layout (`layouts`; `GET_STATE` itself never carries one), and the resolved
/// agent records. Panes join their session through the resources' `session`
/// facet ([`phux_client::resource::terminals_in`]); a session without a
/// layout falls back to one window holding every pane.
pub(super) fn archive_from_snapshot(
    snapshot: &SessionSnapshot,
    agent_sessions: AgentRecords<'_>,
    layouts: &HashMap<SessionId, ArrangementDoc>,
    projects: &HashMap<String, String>,
    hosts: &HashMap<String, String>,
    directories: &HashMap<String, String>,
) -> (WorkspaceArchive, Vec<String>) {
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
            let panes: Vec<&ResourceInfo> =
                phux_client::resource::terminals_in(snapshot, session.id).collect();
            let focus = session_focus(snapshot, session, &panes);
            let windows = layouts.get(&session.id).map_or_else(
                || {
                    registry_windows(
                        session,
                        snapshot,
                        &panes,
                        focus.as_ref(),
                        &resources_by_id,
                        agent_sessions,
                    )
                },
                |layout| {
                    windows_from_layout(
                        session,
                        layout,
                        &panes,
                        &resources_by_id,
                        agent_sessions,
                        &mut warnings,
                    )
                },
            );
            let (pane_cwd, pane_host) = focused_place(focus.as_ref(), &resources_by_id);
            WorkspaceSession {
                name: session.name.clone(),
                active: session.id == snapshot.focused_session,
                cwd: directories.get(&session.name).cloned().or(pane_cwd),
                host: pane_host.or_else(|| hosts.get(&session.name).cloned()),
                project: projects.get(&session.name).cloned(),
                project_link: None,
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

/// The session's focused pane: its reported recent input target
/// (`SESSION_STATE.recent_input`) when it is one of the session's panes,
/// else the legacy window's active pane (read from any server while legacy
/// windows exist, and the session's first pane when it has no recent
/// input; ADR-0161 §9 step 3 removes it), else the snapshot's focused pane
/// when it is in this session, else the session's first pane.
fn session_focus(
    snapshot: &SessionSnapshot,
    session: &SessionInfo,
    panes: &[&ResourceInfo],
) -> Option<ResourceId> {
    let listed = |id: &ResourceId| panes.iter().any(|pane| pane.id == *id);
    session
        .recent_input
        .clone()
        .filter(listed)
        .or_else(|| legacy_focus_hint(snapshot, session))
        .or_else(|| {
            (snapshot.focused_session == session.id)
                .then(|| snapshot.focused_resource.clone())
                .filter(listed)
        })
        .or_else(|| panes.first().map(|pane| pane.id.clone()))
}

/// The active pane of the session's active legacy window. Optional: a
/// snapshot with no windows has none, and focus falls back.
fn legacy_focus_hint(snapshot: &SessionSnapshot, session: &SessionInfo) -> Option<ResourceId> {
    let window_id = session.active_window?;
    snapshot
        .windows
        .iter()
        .find(|window| window.id == window_id && window.session_id == session.id)?
        .active_resource
        .clone()
}

/// The focused pane's directory and satellite host. A local pane has no host.
fn focused_place(
    focus: Option<&ResourceId>,
    resources_by_id: &HashMap<ResourceId, &ResourceInfo>,
) -> (Option<String>, Option<String>) {
    let cwd = focus
        .and_then(|id| resources_by_id.get(id))
        .and_then(|resource| resource.cwd.clone());
    let host = focus.and_then(|id| id.host().map(|host| host.as_str().to_owned()));
    (cwd, host)
}

/// One session's archived windows from its stored layout, first reconciled
/// with the live panes ([`reconcile_layout`]); live panes placed nowhere (a
/// headless spawn, normally) go into a synthesized `"unplaced"` window, with
/// a warning.
fn windows_from_layout(
    session: &SessionInfo,
    layout: &ArrangementDoc,
    panes: &[&ResourceInfo],
    resources_by_id: &HashMap<ResourceId, &ResourceInfo>,
    agent_sessions: AgentRecords<'_>,
    warnings: &mut Vec<String>,
) -> Vec<WorkspaceWindow> {
    let live_ids: Vec<ResourceId> = panes.iter().map(|pane| pane.id.clone()).collect();
    let (kept, unplaced) = reconcile_layout(layout, &live_ids);
    let mut windows: Vec<WorkspaceWindow> = kept
        .into_iter()
        .map(|(active, window)| {
            archive_window_from_layout(&window, active, resources_by_id, agent_sessions)
        })
        .collect();
    if !unplaced.is_empty() {
        warnings.push(format!(
            "archive export: session {:?} has {} live pane(s) absent from its stored layout \
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

/// The registry projection of a session with no stored layout: one active
/// window holding every pane (a session is one window, ADR-0160). A server
/// that does not report its panes' `session` facet predates that model, so
/// its own windows are projected instead ([`legacy_windows`]).
fn registry_windows(
    session: &SessionInfo,
    snapshot: &SessionSnapshot,
    panes: &[&ResourceInfo],
    focus: Option<&ResourceId>,
    resources_by_id: &HashMap<ResourceId, &ResourceInfo>,
    agent_sessions: AgentRecords<'_>,
) -> Vec<WorkspaceWindow> {
    if panes.is_empty() {
        return Vec::new();
    }
    if panes.iter().any(|pane| pane.session.is_none()) {
        return legacy_windows(session, snapshot, resources_by_id, agent_sessions);
    }
    let ids: Vec<ResourceId> = panes.iter().map(|pane| pane.id.clone()).collect();
    vec![WorkspaceWindow {
        name: SESSION_WINDOW_NAME.to_owned(),
        active: true,
        // `GET_STATE` carries no split tree (ADR-0030); only a layout does.
        layout: None,
        panes: archive_panes(&ids, focus, resources_by_id, agent_sessions),
    }]
}

/// One archived window per legacy window of `session`, in window order, for
/// a server that predates the `session` facet.
fn legacy_windows(
    session: &SessionInfo,
    snapshot: &SessionSnapshot,
    resources_by_id: &HashMap<ResourceId, &ResourceInfo>,
    agent_sessions: AgentRecords<'_>,
) -> Vec<WorkspaceWindow> {
    let mut windows: Vec<&WindowInfo> = snapshot
        .windows
        .iter()
        .filter(|window| window.session_id == session.id)
        .collect();
    windows.sort_by_key(|window| window.index);
    windows
        .into_iter()
        .map(|window| {
            let ids: Vec<ResourceId> = snapshot
                .resources
                .iter()
                .filter(|resource| resource.window_id == window.id)
                .map(|resource| resource.id.clone())
                .collect();
            WorkspaceWindow {
                name: window.name.clone(),
                active: Some(window.id) == session.active_window,
                layout: None,
                panes: archive_panes(
                    &ids,
                    window.active_resource.as_ref(),
                    resources_by_id,
                    agent_sessions,
                ),
            }
        })
        .collect()
}

/// Drop every layout leaf not in `live_ids` (the server never prunes closed
/// panes from stored layouts), collapsing each affected split and dropping a
/// window that loses every leaf. Returns the surviving windows (with whether
/// each was active) and, in registry order, the live panes left unplaced.
fn reconcile_layout(
    layout: &ArrangementDoc,
    live_ids: &[ResourceId],
) -> (Vec<(bool, Group)>, Vec<ResourceId>) {
    let live: HashSet<ResourceId> = live_ids.iter().cloned().collect();
    let mut placed: HashSet<ResourceId> = HashSet::new();
    let mut kept = Vec::with_capacity(layout.groups.len());
    for (index, window) in layout.groups.iter().enumerate() {
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
            Group::with_id(
                window.id,
                window.name.clone(),
                LayoutState {
                    focus: window.state.focus.clone().filter(|id| live.contains(id)),
                    tree: Some(tree),
                },
            ),
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
    agent_sessions: AgentRecords<'_>,
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
    window: &Group,
    active: bool,
    resources_by_id: &HashMap<ResourceId, &ResourceInfo>,
    agent_sessions: AgentRecords<'_>,
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
/// per pane; the pane `focus` names is the active one. Shared by every
/// window builder here.
fn archive_panes(
    ids: &[ResourceId],
    focus: Option<&ResourceId>,
    resources_by_id: &HashMap<ResourceId, &ResourceInfo>,
    agent_sessions: AgentRecords<'_>,
) -> Vec<WorkspacePane> {
    ids.iter()
        .map(|id| {
            let info = resources_by_id.get(id).copied();
            WorkspacePane {
                active: Some(id) == focus,
                title: info.and_then(|resource| resource.title.clone()),
                cwd: info.and_then(|resource| resource.cwd.clone()),
                command: None,
                native_launch: agent_sessions.native.get(id).cloned(),
                env: BTreeMap::new(),
                agent_session: if agent_sessions.native.contains_key(id) {
                    None
                } else {
                    agent_sessions
                        .attached
                        .get(id)
                        .map(|record| WorkspaceAgentSession {
                            plugin_id: record.plugin_id.clone(),
                            integration_id: record.integration_id.clone(),
                            native_id: record.native_id.clone(),
                        })
                },
                cols: info.map_or(0, |resource| resource.cols),
                rows: info.map_or(0, |resource| resource.rows),
            }
        })
        .collect()
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
        // ponytail: an archive keeps a stack's shown pane in its slot and the
        // hidden ones as unplaced panes; add a stack node to the archive
        // format when restoring stacks matters.
        LayoutNode::Stack { .. } => phux_client::layout::shown_leaves(layout)
            .first()
            .and_then(|id| pane_index.get(id))
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
mod tests;
