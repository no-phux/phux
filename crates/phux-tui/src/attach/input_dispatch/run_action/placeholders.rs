//! Actions on placeholder leaves: documents, trees, surfaces, and agent
//! panes another client arranged. The TUI owns only their views, so a kill
//! closes the view (as Rune hides them) and never ends the resource or its
//! producer; terminal-only actions bell locally.

use phux_protocol::ResourceId;

use crate::attach::actions;
use crate::attach::input_dispatch::ctx::DispatchCtx;
use crate::attach::input_dispatch::effects::ActionEffects;
use crate::attach::input_dispatch::pickers::window_holding;

/// The placeholder leaves of window `index`.
fn views_in(ctx: &DispatchCtx<'_>, index: usize) -> Vec<ResourceId> {
    ctx.workspace
        .groups
        .get(index)
        .and_then(|group| group.state.tree.as_ref())
        .map(crate::layout::leaves)
        .unwrap_or_default()
        .into_iter()
        .filter(|leaf| ctx.is_surface_leaf(leaf))
        .collect()
}

/// Fold each `(window, view)` out of its window, then drop emptied windows
/// and re-anchor focus. `true` when any view closed.
pub(super) fn close_views(
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
    views: &[(usize, ResourceId)],
) -> bool {
    let mut closed = false;
    for (index, view) in views {
        let Some(group) = ctx.workspace.groups.get_mut(*index) else {
            continue;
        };
        let Ok(state) = actions::apply_terminal_closed(&group.state, view) else {
            continue;
        };
        group.state = state;
        if ctx.zoomed.as_ref() == Some(view) {
            *ctx.zoomed = None;
        }
        closed = true;
    }
    if closed {
        ctx.workspace.prune_empty_groups();
        effects.set_focus = ctx.workspace.active_group().and_then(|ls| ls.focus.clone());
        effects.clear_predict = true;
        effects.layout_mutated = true;
        effects.set_metadata = true;
    }
    closed
}

/// Close every placeholder view in the active window. `true` when any did.
pub(super) fn close_active_window_views(
    ctx: &mut DispatchCtx<'_>,
    effects: &mut ActionEffects,
) -> bool {
    let active = ctx.workspace.active;
    let views: Vec<_> = views_in(ctx, active)
        .into_iter()
        .map(|view| (active, view))
        .collect();
    close_views(ctx, effects, &views)
}

/// The views a confirmed `kill-window` closes: the placeholders sharing a
/// window with the killed `terminals` when every terminal there dies, plus
/// any placeholder named as a target itself.
pub(super) fn views_closed_by_kill(
    ctx: &DispatchCtx<'_>,
    terminals: &[ResourceId],
    named_views: &[ResourceId],
) -> Vec<(usize, ResourceId)> {
    let mut views: Vec<(usize, ResourceId)> = Vec::new();
    for (index, group) in ctx.workspace.groups.iter().enumerate() {
        let leaves = group
            .state
            .tree
            .as_ref()
            .map(crate::layout::leaves)
            .unwrap_or_default();
        let (placeholders, panes): (Vec<_>, Vec<_>) = leaves
            .into_iter()
            .partition(|leaf| ctx.is_surface_leaf(leaf));
        let dies = !panes.is_empty() && panes.iter().all(|pane| terminals.contains(pane));
        if dies {
            views.extend(placeholders.into_iter().map(|view| (index, view)));
        }
    }
    for view in named_views {
        if let Some(index) = window_holding(ctx.workspace, view)
            && !views.iter().any(|(_, known)| known == view)
        {
            views.push((index, view.clone()));
        }
    }
    views
}

/// A terminal-only action (copy mode, input lease, signal) aimed at a
/// placeholder: bell instead of a command the server would refuse. `true`
/// when it was one.
pub(super) fn refuse_on_placeholder(
    ctx: &DispatchCtx<'_>,
    effects: &mut ActionEffects,
    focused: Option<&ResourceId>,
    action: &str,
) -> bool {
    let Some(focused) = focused.filter(|id| ctx.is_surface_leaf(id)) else {
        return false;
    };
    tracing::debug!(terminal = ?focused, action, "terminal-only action on a placeholder");
    effects.bell = true;
    true
}

/// The members `reopen-views` may show again: the session's terminals and
/// the documents, trees, and surfaces it holds.
pub(super) fn reopenable_members(ctx: &DispatchCtx<'_>) -> std::collections::HashSet<ResourceId> {
    let mut members = ctx.session_members.clone();
    if let Some(session) = ctx.peers.focused_session {
        members.extend(ctx.leaves.documents_in(session));
    }
    members
}
