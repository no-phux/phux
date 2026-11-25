//! Chrome pointer ownership: sidebar/tab clicks and menus, plus divider and
//! window drag admission, motion, release and cancellation.

use crate::attach::input_dispatch::args::{bare_action, tab_click_action};
use crate::attach::input_dispatch::effects::broadcast_layout;

use super::super::{
    AttachError, ContextMenu, DispatchCtx, DividerGrab, DragGrab, EventEnv, MouseAction,
    MouseEvent, SidebarHit, StageOutcome, WindowStrip, actions, chrome_drag, content_rect,
    hit_test, sidebar_click_action,
};
use super::{is_left_press, is_right_press, quantize_cell};

#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
impl<W: crate::attach::RenderSink> EventEnv<'_, '_, W> {
    /// Advance (or end) an in-flight chrome drag: a pane divider, the
    /// sidebar edge, or a window tab or row. `None` when nothing is grabbed
    /// and the event should route normally.
    pub(super) async fn step_chrome_drag(
        &mut self,
        mouse: &MouseEvent,
    ) -> Result<Option<StageOutcome>, AttachError> {
        let Some(grab) = self.ctx.drag.clone() else {
            return Ok(None);
        };
        match mouse.action {
            // ADR-0048: a release always ends the grab, wherever it lands, and
            // publishes the resulting layout.
            MouseAction::Release => {
                *self.ctx.drag = None;
                let commit = chrome_drag::release(self.ctx, &grab, mouse);
                if let Some(target) = &commit.focus {
                    self.focus_pane_from_click(target);
                }
                if commit.broadcast {
                    broadcast_layout(self.conn, self.ctx).await?;
                }
                tracing::debug!(?grab, "chrome drag: released");
                Ok(Some(StageOutcome::consumed(commit.layout_changed)))
            }
            // While something is grabbed, motion advances it and nothing
            // reaches a pane.
            MouseAction::Motion => Ok(Some(StageOutcome::consumed(chrome_drag::motion(
                self.ctx, &grab, mouse,
            )))),
            // Anything else mid-drag (chorded press, wheel) is consumed.
            MouseAction::Press => {
                tracing::trace!(
                    action = ?mouse.action,
                    button = ?mouse.button,
                    "dropping mouse event during chrome drag"
                );
                Ok(Some(StageOutcome::CONSUMED))
            }
        }
    }

    /// End a drag whose release will never arrive (the outer terminal lost
    /// focus), so the next press is not eaten. A divider keeps and publishes
    /// its ratio; a window drag is cancelled. True when a marker must erase.
    pub(in crate::attach::input_dispatch::dispatch) async fn abandon_chrome_drag(
        &mut self,
    ) -> Result<bool, AttachError> {
        let Some(grab) = self.ctx.drag.take() else {
            return Ok(false);
        };
        if matches!(grab, DragGrab::Divider(_)) {
            broadcast_layout(self.conn, self.ctx).await?;
            tracing::debug!(?grab, "chrome drag: abandoned on focus loss");
            return Ok(false);
        }
        tracing::debug!(?grab, "chrome drag: abandoned on focus loss");
        Ok(matches!(grab, DragGrab::Window(_)))
    }

    /// The sidebar strip claims every pointer event over its cells: a left
    /// press commits its row (or grabs the edge), a right press a menu.
    pub(super) async fn route_sidebar_click(
        &mut self,
        mouse: &MouseEvent,
    ) -> Result<Option<StageOutcome>, AttachError> {
        let Some(res) = self.ctx.sidebar else {
            return Ok(None);
        };
        let strip = crate::attach::paint::sidebar_rect(self.ctx.viewport, res);
        let (cell_x, cell_y) = (quantize_cell(mouse.x), quantize_cell(mouse.y));
        if !strip_contains(strip, cell_x, cell_y) {
            return Ok(None);
        }
        let hit = sidebar_click_action(strip, self.ctx.sidebar_targets, cell_x, cell_y);
        let mut layout_changed = false;
        if is_left_press(mouse) {
            layout_changed = self
                .sidebar_left_press(strip, hit, (cell_x, cell_y))
                .await?;
        } else if is_right_press(mouse) {
            // A session row manages that exact destination without switching first.
            if let Some(mut row) = hit
                .clone()
                .filter(|row| matches!(row.action.as_str(), "switch-session" | "switch-host"))
            {
                row.args.insert(
                    "switch-action".to_owned(),
                    toml::Value::String(row.action.clone()),
                );
                "session-actions".clone_into(&mut row.action);
                layout_changed = self.run_resolved(&row).await?;
            } else {
                let window_row = hit.filter(|r| r.action == "select-window");
                layout_changed = self
                    .open_chrome_context_menu(window_row, (cell_x, cell_y))
                    .await?;
            }
        }
        Ok(Some(StageOutcome::consumed(layout_changed)))
    }

    /// A left press on the sidebar strip: the pane-facing edge starts a
    /// resize; any other target commits its action, and a window row is
    /// also picked up so releasing it over another window row reorders.
    async fn sidebar_left_press(
        &mut self,
        strip: crate::layout::Rect,
        hit: Option<phux_config::keybind::ResolvedAction>,
        (cell_x, cell_y): (u16, u16),
    ) -> Result<bool, AttachError> {
        if chrome_drag::on_sidebar_edge(self.ctx, cell_x, cell_y) {
            chrome_drag::begin_sidebar_resize(self.ctx);
            return Ok(false);
        }
        let layout_changed = if let Some(resolved) = hit {
            tracing::debug!(action = %resolved.action, "sidebar: click dispatched");
            self.run_resolved(&resolved).await?
        } else {
            false
        };
        if let Some(SidebarHit::Window(index)) =
            hit_test(strip, self.ctx.sidebar_targets.counts, cell_x, cell_y)
        {
            chrome_drag::begin_window_drag(self.ctx, index, WindowStrip::Sidebar);
        }
        Ok(layout_changed)
    }

    /// The status-bar row is chrome: a left press on a tab selects (and
    /// grabs) it, a right press opens a menu, the rest is consumed.
    pub(super) async fn route_status_bar_click(
        &mut self,
        mouse: &MouseEvent,
    ) -> Result<Option<StageOutcome>, AttachError> {
        let Some(bar_row) = chrome_drag::bar_row(self.ctx) else {
            return Ok(None);
        };
        let (cell_x, cell_y) = (quantize_cell(mouse.x), quantize_cell(mouse.y));
        if cell_y != bar_row {
            return Ok(None);
        }
        let hit = bar_click_action(self.ctx.status_bar, cell_x);
        let mut layout_changed = false;
        if is_left_press(mouse) {
            if let Some(resolved) = hit {
                tracing::debug!(action = %resolved.action, "status bar: tab click dispatched");
                layout_changed = self.run_resolved(&resolved).await?;
            }
            // A press on a tab also picks the window up; releasing over
            // another tab drops it into that slot.
            if let Some(index) = self
                .ctx
                .status_bar
                .and_then(|bar| bar.window_hit_at(cell_x))
            {
                chrome_drag::begin_window_drag(self.ctx, index, WindowStrip::Tabs);
            }
        } else if is_right_press(mouse) {
            // A tab is selected and gets the window menu; elsewhere, the
            // session menu (clamped into the content rect).
            layout_changed = self.open_chrome_context_menu(hit, (cell_x, cell_y)).await?;
        }
        Ok(Some(StageOutcome::consumed(layout_changed)))
    }

    /// Commit `window_row` when given and open the window menu, else the
    /// session menu, at `anchor`.
    async fn open_chrome_context_menu(
        &mut self,
        window_row: Option<phux_config::keybind::ResolvedAction>,
        anchor: (u16, u16),
    ) -> Result<bool, AttachError> {
        let is_window = window_row.is_some();
        let layout_changed = match window_row {
            Some(resolved) => self.run_resolved(&resolved).await?,
            None => false,
        };
        let spec = if is_window {
            crate::attach::context_menu::active_window_menu(self.ctx)
        } else {
            crate::attach::context_menu::session_menu(
                Some(self.ctx.keybindings),
                self.ctx.session_name,
            )
        };
        open_context_menu(self.ctx, spec, anchor);
        Ok(layout_changed)
    }

    /// ADR-0048: a left press on a divider grabs it and snaps the split to
    /// the pointer; other presses on a divider are dropped.
    pub(super) fn grab_divider(
        &mut self,
        mouse: &MouseEvent,
        node_path: crate::layout::NodePath,
        axis: crate::layout::SplitDir,
    ) -> bool {
        if !self.ctx.layout_read_complete || !is_left_press(mouse) {
            tracing::trace!(x = mouse.x, y = mouse.y, "dropping mouse on divider");
            return false;
        }
        let grab = DividerGrab { node_path, axis };
        let layout_changed = drag_resize(self.ctx, mouse, &grab);
        *self.ctx.drag = Some(DragGrab::Divider(grab));
        tracing::debug!("divider drag: grabbed");
        layout_changed
    }
}

/// Apply one drag step through [`actions::apply_divider_resize`] (the same
/// floor math as keyboard resize); true iff the layout changed. A floor hit
/// or stale grab leaves the divider where it is.
pub(in crate::attach::input_dispatch) fn drag_resize(
    ctx: &mut DispatchCtx<'_>,
    mouse: &MouseEvent,
    grab: &DividerGrab,
) -> bool {
    // Snapshot the geometry that feeds the resize before borrowing the
    // workspace mutably for the active window.
    let viewport = ctx.viewport;
    let bar = ctx.bar;
    let sidebar = ctx.sidebar;
    let Some(ls) = ctx.workspace.active_group_mut() else {
        return false;
    };
    let pointer = (quantize_cell(mouse.x), quantize_cell(mouse.y));
    match actions::apply_divider_resize(
        ls,
        &grab.node_path,
        grab.axis,
        pointer,
        viewport,
        bar,
        sidebar,
    ) {
        Ok(Some(new_state)) => {
            *ls = new_state;
            true
        }
        // Min-cell floor or stale grab — keep the divider where it is.
        Ok(None) | Err(_) => false,
    }
}

/// Whether an outer-viewport cell lies within the sidebar
/// strip's rect (separator column included — the strip consumes it even
/// though it is not a hit target).
const fn strip_contains(rect: crate::layout::Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.w)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.h)
}

/// Map a press on the status-bar row to its action: a tab commits
/// `select-window`, the `switch` chip the fleet, a navigation hint its
/// action. The hit test lives with the painter, so targets match the paint.
pub(in crate::attach::input_dispatch) fn bar_click_action(
    painter: Option<&crate::render::chrome::status_bar::StatusBarPainter>,
    x: u16,
) -> Option<phux_config::keybind::ResolvedAction> {
    use crate::render::chrome::status_bar::widgets::CellHit;
    match painter?.hit_at(x)? {
        CellHit::Window(index) => tab_click_action(index),
        CellHit::Switch => Some(bare_action("agent-fleet")),
        CellHit::Action(action) => Some(bare_action(action)),
    }
}

/// Push `spec` as a context menu at `anchor` (ADR-0058), clamped inside the
/// pane content rect so it never covers the sidebar or bar.
pub(in crate::attach::input_dispatch) fn open_context_menu(
    ctx: &mut DispatchCtx<'_>,
    spec: crate::attach::context_menu::MenuSpec,
    anchor: (u16, u16),
) {
    let area = content_rect(ctx.viewport, ctx.bar, ctx.sidebar);
    tracing::debug!(
        title = %spec.title,
        rows = spec.rows.len(),
        anchor_x = anchor.0,
        anchor_y = anchor.1,
        "context menu: opened",
    );
    ctx.overlays.push(Box::new(ContextMenu::new(
        spec.title, spec.rows, anchor, area, ctx.theme,
    )));
}
