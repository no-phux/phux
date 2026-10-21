//! Windows and layouts: spawned windows and reconciling a loaded arrangement.

use std::collections::HashMap;

use phux_client::layout_ops::{
    DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, LayoutKeyOwner, layout_key_session,
};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{Scope, SpawnResult};

use super::FrameOutcome;
use super::replies::open_window;
use crate::attach::actions;
use crate::attach::actions::{Adopt, ParkedAdopt, PendingWindow, SpawnedPane};
use crate::attach::outcome::AttachError;
use crate::attach::pane_state::PaneSlot;
use crate::layout::{ArrangementDoc, LayoutState};

/// Apply a `RESOURCE_SPAWNED` reply for a parked `new-window`: append an
/// active window on the new pane, seed its slot, focus it, and request
/// repaint, broadcast, and reflow.
pub(in crate::attach::server_frame) fn handle_window_spawned<W: crate::attach::RenderSink>(
    out: &mut W,
    workspace: &mut ArrangementDoc,
    focused_resource: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    pending: &PendingWindow,
    result: SpawnResult,
) -> Result<FrameOutcome, AttachError> {
    let instance = result.instance();
    match result {
        // A satellite spawn streams to no one until attached: park the window
        // on that attach, token included.
        SpawnResult::Ok(new_id) | SpawnResult::OkBound { id: new_id, .. } if !new_id.is_local() => {
            Ok(FrameOutcome {
                adopt_spawned: vec![ParkedAdopt::Window(PendingWindow {
                    name: pending.name.clone(),
                    adopt: Some(Adopt::Spawned(SpawnedPane {
                        id: new_id,
                        instance,
                    })),
                })],
                ..FrameOutcome::default()
            })
        }
        SpawnResult::Ok(new_id) | SpawnResult::OkBound { id: new_id, .. } => {
            open_window(workspace, focused_resource, panes, &pending.name, new_id)
        }
        SpawnResult::Err(err) => {
            tracing::warn!(error = ?err, "new-window: server-side spawn failed");
            let _ = actions::write_bell(out);
            Ok(FrameOutcome::default())
        }
        // SpawnResult is #[non_exhaustive] — tolerate future variants.
        _ => {
            tracing::warn!("new-window: unknown SpawnResult variant");
            Ok(FrameOutcome::default())
        }
    }
}

/// The focused session's display name from an `ATTACHED` snapshot, or empty.
pub(super) fn focused_session_name(
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
) -> String {
    snapshot
        .sessions
        .iter()
        .find(|s| s.id == snapshot.focused_session)
        .map(|s| s.name.clone())
        .unwrap_or_default()
}

/// Which session an ADR-0019 layout key (`phux.tui.layout/v1[/<session>]`,
/// default Group) names; `None` ⇒ not a layout key we can attribute. The
/// caller must know whose layout arrived before adopting it.
pub(super) fn layout_key_scope_session(scope: &Scope, key: &str) -> Option<LayoutKeyOwner> {
    if !matches!(scope, Scope::Group(id) if *id == DEFAULT_GROUP_ID) {
        return None;
    }
    layout_key_session(key)
}

/// Whether a persisted layout names panes and none is in the `ATTACHED`
/// snapshot (ADR-0105): a stale tree of dead panes that would hide the empty
/// state.
pub(super) fn layout_names_only_absent_panes(
    incoming: &ArrangementDoc,
    panes: &HashMap<ResourceId, PaneSlot>,
) -> bool {
    let mut leaves = incoming
        .groups
        .iter()
        .filter_map(|window| window.state.tree.as_ref())
        .flat_map(crate::layout::leaves)
        .peekable();
    leaves.peek().is_some() && leaves.all(|leaf| !panes.contains_key(&leaf))
}

pub(super) fn unknown_layout_leaves(
    incoming: &ArrangementDoc,
    panes: &HashMap<ResourceId, PaneSlot>,
) -> Vec<ResourceId> {
    incoming
        .groups
        .iter()
        .filter_map(|window| window.state.tree.as_ref())
        .flat_map(crate::layout::leaves)
        .filter(|terminal| !panes.contains_key(terminal))
        .collect()
}

/// Reconcile a workspace whose session ownership was established by its key or
/// correlated GET. Terminal overlap and replica admission are not authority.
pub(in crate::attach::server_frame) fn reconcile_loaded_workspace(
    mut incoming: ArrangementDoc,
    local: &ArrangementDoc,
    bootstrap_focus: Option<&ResourceId>,
) -> ArrangementDoc {
    for window in &mut incoming.groups {
        let local_focus = local
            .groups
            .iter()
            .find(|old| old.id == window.id)
            .and_then(|local_window| local_window.state.focus.as_ref())
            .or(bootstrap_focus);
        reconcile_loaded_layout(&mut window.state, local_focus);
    }
    incoming.active = reconciled_active_window(&incoming, local, bootstrap_focus);
    incoming
}

fn reconciled_active_window(
    incoming: &ArrangementDoc,
    local: &ArrangementDoc,
    bootstrap_focus: Option<&ResourceId>,
) -> usize {
    let active_id = local.groups.get(local.active).map(|window| window.id);
    incoming
        .groups
        .iter()
        .position(|window| Some(window.id) == active_id)
        .or_else(|| window_containing_focus(incoming, bootstrap_focus))
        .unwrap_or_else(|| local.active.min(incoming.groups.len().saturating_sub(1)))
}

fn window_containing_focus(
    workspace: &ArrangementDoc,
    focus: Option<&ResourceId>,
) -> Option<usize> {
    let focus = focus?;
    workspace.groups.iter().position(|window| {
        window
            .state
            .tree
            .as_ref()
            .is_some_and(|tree| crate::layout::leaves(tree).contains(focus))
    })
}

/// Preserve a valid local focus while adopting `state`'s tree; otherwise the
/// first depth-first leaf (ADR-0019).
fn reconcile_loaded_layout(state: &mut LayoutState, local_focus: Option<&ResourceId>) {
    let tree_leaves = state
        .tree
        .as_ref()
        .map(crate::layout::leaves)
        .unwrap_or_default();
    state.focus = local_focus
        .filter(|focus| tree_leaves.contains(focus))
        .cloned()
        .or_else(|| tree_leaves.into_iter().next());
}

#[cfg(test)]
mod session_name_tests {
    use super::focused_session_name;
    use phux_protocol::ids::{ResourceId, SessionId, WindowId};
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};

    fn snapshot_with(sessions: Vec<SessionInfo>, focused: SessionId) -> SessionSnapshot {
        SessionSnapshot::new(focused, WindowId::new(0), ResourceId::local(0))
            .with_sessions(sessions)
    }

    #[test]
    fn focused_session_name_resolves_the_matching_session() {
        // The widget reads the name of the focused session,
        // not the first session in the list.
        let snapshot = snapshot_with(
            vec![
                SessionInfo::new(SessionId::new(1), "work"),
                SessionInfo::new(SessionId::new(2), "play"),
            ],
            SessionId::new(2),
        );
        assert_eq!(focused_session_name(&snapshot), "play");
    }

    #[test]
    fn focused_session_name_is_empty_when_focus_is_absent() {
        // Degrade to an empty widget rather than panic if the focused
        // session somehow isn't in the list.
        let snapshot = snapshot_with(
            vec![SessionInfo::new(SessionId::new(1), "work")],
            SessionId::new(99),
        );
        assert_eq!(focused_session_name(&snapshot), "");
    }
}
