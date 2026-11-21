//! `display-panes` and `focus-sidebar`: pick a pane by its number, or a
//! sidebar row, from the keyboard.

use crate::render::overlay::{PaneNumbers, SidebarCursor};

use super::super::args::{focus_pane_action, select_window_action};
use super::ActionCall;

/// Number the tab's panes until the next key; a digit focuses that pane.
pub(in crate::attach) fn display_panes(ActionCall { ctx, effects, .. }: ActionCall<'_, '_>) {
    let window = ctx.workspace.active;
    let panes = ctx
        .workspace
        .active_group()
        .and_then(|group| group.tree.as_ref())
        .map(crate::layout::leaves)
        .unwrap_or_default();
    if panes.is_empty() {
        effects.bell = true;
        return;
    }
    let picks = (0..panes.len())
        .filter_map(|pane| focus_pane_action(window, pane))
        .collect();
    ctx.overlays.push(Box::new(PaneNumbers::new(panes, picks)));
}

/// Move the keyboard into the sidebar, showing it first when hidden. The
/// cursor starts on the tab in view.
pub(in crate::attach) fn focus_sidebar(call: ActionCall<'_, '_>) {
    let ActionCall { ctx, effects, .. } = call;
    if !*ctx.sidebar_enabled {
        let fits = crate::attach::paint::sidebar_reservation(
            ctx.viewport.0,
            true,
            *ctx.sidebar_width,
            crate::attach::paint::SidebarEdge::Left,
            ctx.chrome.min_pane_cols,
        )
        .is_some();
        if !fits {
            effects.bell = true;
            return;
        }
        effects.toggle_sidebar = true;
        effects.layout_mutated = true;
    }
    let start = select_window_action(ctx.workspace.active);
    ctx.overlays.push(Box::new(SidebarCursor::new(start)));
}
