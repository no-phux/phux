//! Arrangement edits that arrived before the initial layout read.
//!
//! `close-view` and `reopen-views` change the shared arrangement. Applying
//! them to the bootstrap fallback, then skipping the save because the read
//! fence is down, drops the edit: detach and reattach show only the
//! arrangement the session was created with (#1475). The keystroke is held
//! and applied to the workspace the read adopts, then saved. Nothing here
//! places a member the user did not ask to show.

use std::collections::{HashMap, HashSet};

use phux_protocol::ResourceId;

use crate::attach::pane_state::PaneSlot;
use crate::layout::ArrangementDoc;

/// One arrangement edit held until the layout read completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::attach) enum DeferredArrangementEdit {
    /// Fold `target` out of whatever window holds it after the read.
    CloseView { target: ResourceId },
    /// Show every session member the adopted arrangement leaves out.
    ReopenViews,
}

/// Apply `edits` to the workspace the layout read adopted.
///
/// Returns whether the document changed, which is the caller's cue to save
/// it. An edit that names a pane the adopted layout does not hold does
/// nothing. Members the user did not ask to reopen stay hidden.
pub(in crate::attach) fn apply_deferred_arrangement_edits(
    edits: &mut Vec<DeferredArrangementEdit>,
    workspace: &mut ArrangementDoc,
    members: &HashSet<ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
    zoomed: &mut Option<ResourceId>,
    focused: &mut Option<ResourceId>,
) -> bool {
    let pending = std::mem::take(edits);
    let mut changed = false;
    for edit in pending {
        changed |= match edit {
            DeferredArrangementEdit::CloseView { target } => {
                apply_close_view(workspace, &target, zoomed, focused)
            }
            DeferredArrangementEdit::ReopenViews => {
                apply_reopen_views(workspace, members, panes, focused)
            }
        };
    }
    changed
}

/// Fold `target` out of its window. `false` when the adopted layout does
/// not show it.
fn apply_close_view(
    workspace: &mut ArrangementDoc,
    target: &ResourceId,
    zoomed: &mut Option<ResourceId>,
    focused: &mut Option<ResourceId>,
) -> bool {
    let Some(index) = super::pickers::window_holding(workspace, target) else {
        tracing::info!(
            action = "close-view",
            reason = "pane not in the adopted layout",
            "arrangement save skipped"
        );
        return false;
    };
    match crate::attach::actions::apply_terminal_closed(&workspace.groups[index].state, target) {
        Ok(state) => workspace.groups[index].state = state,
        Err(err) => {
            tracing::info!(
                action = "close-view",
                reason = %err,
                "arrangement save skipped"
            );
            return false;
        }
    }
    workspace.prune_empty_groups();
    if zoomed.as_ref() == Some(target) {
        *zoomed = None;
    }
    *focused = workspace
        .active_group()
        .and_then(|group| group.focus.clone());
    true
}

/// One window per member the adopted arrangement does not show.
fn apply_reopen_views(
    workspace: &mut ArrangementDoc,
    members: &HashSet<ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
    focused: &mut Option<ResourceId>,
) -> bool {
    let hidden = crate::attach::session_mirror::unarranged_members(members, workspace, panes);
    if hidden.is_empty() {
        tracing::info!(
            action = "reopen-views",
            reason = "nothing hidden in the adopted layout",
            "arrangement save skipped"
        );
        return false;
    }
    let first = workspace.groups.len();
    for id in hidden {
        let name = workspace.default_group_name();
        workspace.add_group(name, id);
    }
    let _ = workspace.select(first);
    *focused = workspace
        .active_group()
        .and_then(|group| group.focus.clone());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::input_dispatch::test_support::tid;

    #[test]
    fn a_queued_reopen_places_only_the_members_the_user_asked_for() {
        let mut workspace = ArrangementDoc::single(tid(1));
        let members = HashSet::from([tid(1), tid(2), tid(4)]);
        let mut edits = vec![DeferredArrangementEdit::ReopenViews];
        let mut focused = Some(tid(1));
        let mut zoomed = None;
        let changed = apply_deferred_arrangement_edits(
            &mut edits,
            &mut workspace,
            &members,
            &HashMap::new(),
            &mut zoomed,
            &mut focused,
        );
        assert!(changed);
        assert!(edits.is_empty());
        assert_eq!(workspace.groups.len(), 3);
        assert_eq!(focused, Some(tid(2)));
    }

    #[test]
    fn a_queued_close_of_a_pane_the_adopted_layout_lacks_does_not_save() {
        let mut workspace = ArrangementDoc::single(tid(1));
        let before = workspace.clone();
        let mut edits = vec![DeferredArrangementEdit::CloseView { target: tid(9) }];
        let mut focused = Some(tid(1));
        let mut zoomed = Some(tid(1));
        let changed = apply_deferred_arrangement_edits(
            &mut edits,
            &mut workspace,
            &HashSet::new(),
            &HashMap::new(),
            &mut zoomed,
            &mut focused,
        );
        assert!(!changed);
        assert_eq!(workspace, before);
        assert_eq!(zoomed, Some(tid(1)));
    }
}
