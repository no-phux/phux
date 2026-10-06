//! Mouse routing for the dispatcher: the pointer stage of
//! [`EventEnv::dispatch_event`], chrome hit-tests (sidebar strip, status-bar
//! row, pane dividers), and the pane-level gestures the client claims
//! (click-to-focus, wheel, context menu, drag-to-copy).
//!
//! A child of `dispatch` so it can extend [`EventEnv`] and read the stage
//! types without widening their visibility.

use super::super::args::{
    bare_action, focus_pane_action, select_window_action, switch_host_action,
};
use super::super::effects::broadcast_layout;
use super::{
    AttachError, ContextMenu, DispatchCtx, DividerGrab, DragGrab, EventEnv, InputEvent, ModSet,
    MouseAction, MouseButton, MouseEvent, PhysicalKey, Point, PointCoordinate, PointSpace,
    ResourceId, ScreenSelectionPoint, SidebarHit, StageOutcome, WindowStrip, actions, chrome_drag,
    content_rect, focused_pane_rect, hit_test, make_named_key, published_terminal,
    reanchor_predict_to_pane, replica_scroll_moved, scale_to_surface_pixels, switch_session_args,
    terminal_in_alt_screen, terminal_wants_mouse_tracking, wheel_scroll_delta,
};

#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
impl<W: crate::attach::RenderSink> EventEnv<'_, '_, W> {
    /// ADR-0048 pointer stage: chrome drags, sidebar and bar clicks, then
    /// hit-testing the pane composition. A divider press grabs its split; a
    /// pane press focuses and forwards (pane-local), so mouse-tracking apps
    /// still get every event over their cells. Claims every mouse event.
    pub(super) async fn route_mouse_input(
        &mut self,
        ev: &InputEvent,
    ) -> Result<StageOutcome, AttachError> {
        use crate::attach::multi_pane::{RouteDecision, route_mouse_event};
        let InputEvent::Mouse(mouse) = ev else {
            return Ok(StageOutcome::PASS);
        };
        if let Some(outcome) = self.step_chrome_drag(mouse).await? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.route_floating_mouse(mouse).await? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.route_sidebar_click(mouse).await? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.route_status_bar_click(mouse).await? {
            return Ok(outcome);
        }
        // Hit-test the same inset content rect the renderer tiles into (bar
        // and sidebar folded off), or clicks land a row/strip off.
        let content = content_rect(self.ctx.viewport, self.ctx.bar, self.ctx.sidebar);
        // Hit-test the render layout, so a zoomed pane receives the click.
        let decision = {
            let Some(render_ls) = self.ctx.workspace.render_window(self.ctx.zoomed.as_ref()) else {
                tracing::debug!("dropping mouse event: no active window");
                return Ok(StageOutcome::CONSUMED);
            };
            route_mouse_event(&render_ls, content, self.ctx.viewport, mouse)
        };
        match decision {
            RouteDecision::Pane {
                target,
                pane_x,
                pane_y,
                focus_changed,
            } => {
                self.route_mouse_to_pane(mouse, target, (pane_x, pane_y), focus_changed)
                    .await
            }
            RouteDecision::Divider { node_path, axis } => Ok(StageOutcome::consumed(
                self.grab_divider(mouse, node_path, axis),
            )),
            RouteDecision::Miss => {
                tracing::trace!(x = mouse.x, y = mouse.y, "dropping mouse: no target");
                Ok(StageOutcome::CONSUMED)
            }
            RouteDecision::NoFocus => {
                tracing::debug!("dropping mouse event before ATTACHED");
                Ok(StageOutcome::CONSUMED)
            }
        }
    }

    /// ADR-0147: while the floating overlay is open it owns the pointer. Its
    /// interior gets the event pane-local (the wheel scrolls it as it would a
    /// tile), its border swallows it, and a press anywhere else dismisses it.
    /// `None` when no overlay is open.
    async fn route_floating_mouse(
        &mut self,
        mouse: &MouseEvent,
    ) -> Result<Option<StageOutcome>, AttachError> {
        use crate::attach::floating::{floating_box, floating_pane, rect_contains};
        let Some(id) = floating_pane(self.panes).cloned() else {
            return Ok(None);
        };
        let frame = floating_box(content_rect(
            self.ctx.viewport,
            self.ctx.bar,
            self.ctx.sidebar,
        ));
        let (cell_x, cell_y) = (quantize_cell(mouse.x), quantize_cell(mouse.y));
        if rect_contains(frame.inner, cell_x, cell_y) {
            let mut routed = *mouse;
            routed.x -= f64::from(frame.inner.x);
            routed.y -= f64::from(frame.inner.y);
            if let Some(scrolled) = self.scroll_pane_wheel(&id, &routed).await? {
                return Ok(Some(StageOutcome::consumed(scrolled)));
            }
            if !crate::attach::pane_state::pane_exited(self.panes, &id) {
                self.send_terminal_input(
                    id,
                    InputEvent::Mouse(scale_to_surface_pixels(routed, self.ctx.cell_px)),
                    false,
                )
                .await?;
            }
            return Ok(Some(StageOutcome::CONSUMED));
        }
        let outside_press = matches!(mouse.action, MouseAction::Press)
            && !rect_contains(frame.outer, cell_x, cell_y)
            && wheel_scroll_delta(mouse).is_none();
        if !outside_press {
            return Ok(Some(StageOutcome::CONSUMED));
        }
        let dismissed = self.dismiss_floating().await?;
        Ok(Some(StageOutcome::consumed(dismissed)))
    }

    /// Advance (or end) an in-flight chrome drag: a pane divider, the
    /// sidebar edge, or a window tab or row. `None` when nothing is grabbed
    /// and the event should route normally.
    async fn step_chrome_drag(
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
    pub(super) async fn abandon_chrome_drag(&mut self) -> Result<bool, AttachError> {
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

    /// The sidebar strip claims every pointer event over its cells before
    /// pane routing. A left press commits the row's action through
    /// `run_action` (or grabs the edge); a right press opens a menu;
    /// everything else is consumed. `None` when not over the strip.
    async fn route_sidebar_click(
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
            // A window row is selected first, then gets the window menu;
            // anywhere else on the strip gets the session menu.
            let window_row = hit.filter(|r| r.action == "select-window");
            layout_changed = self
                .open_chrome_context_menu(window_row, (cell_x, cell_y))
                .await?;
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

    /// The status-bar row is chrome: a left press on a tab commits its
    /// `select-window` (and picks it up for dragging), a right press opens a
    /// menu, and everything else is consumed. The sidebar hit-tests first,
    /// so by here the event is in the bar's own span. `None` off the row.
    async fn route_status_bar_click(
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
            crate::attach::context_menu::window_menu(
                self.ctx.keybindings,
                &active_window_name(self.ctx),
            )
        } else {
            crate::attach::context_menu::session_menu(self.ctx.keybindings, self.ctx.session_name)
        };
        open_context_menu(self.ctx, spec, anchor);
        Ok(layout_changed)
    }

    /// Route a press inside a pane: click to focus, then the gestures the
    /// client claims (wheel, menu, drag-to-copy), else `INPUT_MOUSE`.
    async fn route_mouse_to_pane(
        &mut self,
        mouse: &MouseEvent,
        target: ResourceId,
        pane_xy: (f64, f64),
        focus_changed: bool,
    ) -> Result<StageOutcome, AttachError> {
        // Heavy-edge chrome moves with focus; repaint
        // dividers + all leaves so the focused pane's
        // surrounding edges render heavy.
        let layout_changed = focus_changed;
        if focus_changed {
            self.focus_pane_from_click(&target);
        }
        // An opted-out pane gets no client-synthesized mouse; click-to-focus
        // above still applies (it is how outer capture drops).
        if self.ctx.mouse_optout.contains(&target) {
            tracing::trace!(
                terminal = ?target,
                "dropping mouse event: pane opted out (set-pane mouse off)"
            );
            return Ok(StageOutcome::consumed(layout_changed));
        }
        let mut routed = *mouse;
        routed.x = pane_xy.0;
        routed.y = pane_xy.1;
        if let Some(scrolled) = self.scroll_pane_wheel(&target, &routed).await? {
            return Ok(StageOutcome::consumed(layout_changed || scrolled));
        }
        if self.open_pane_context_menu(mouse, &target) {
            return Ok(StageOutcome::consumed(layout_changed));
        }
        if self.begin_drag_to_copy(&routed, &target) {
            return Ok(StageOutcome::consumed(layout_changed));
        }
        // ADR-0124: scrolling and copying a retained pane still work above;
        // a mouse report has no process left to read it.
        if crate::attach::pane_state::pane_exited(self.panes, &target)
            || crate::attach::pane_state::pane_satellite_down(self.panes, &target)
        {
            return Ok(StageOutcome::consumed(layout_changed));
        }
        self.send_terminal_input(
            target,
            InputEvent::Mouse(scale_to_surface_pixels(routed, self.ctx.cell_px)),
            false,
        )
        .await?;
        Ok(StageOutcome::consumed(layout_changed))
    }

    /// Move client-local focus to the clicked pane.
    fn focus_pane_from_click(&mut self, target: &ResourceId) {
        if let Some(ls) = self.ctx.workspace.active_window_mut() {
            ls.focus = Some(target.clone());
        }
        self.ctx
            .focus_history
            .transition(self.focused_resource, Some(target.clone()));
        // Re-anchor predict so the next keystroke echoes in the new pane.
        reanchor_predict_to_pane(self.predict, self.panes, target);
    }

    /// Handle a wheel notch over `target`. `None` when the event is not a
    /// wheel notch the client claims (it forwards to the pane instead);
    /// `Some(layout_changed)` when the client consumed it.
    async fn scroll_pane_wheel(
        &mut self,
        target: &ResourceId,
        routed: &MouseEvent,
    ) -> Result<Option<bool>, AttachError> {
        let Some(delta) = wheel_scroll_delta(routed) else {
            return Ok(None);
        };
        let Some(modes) = pane_scroll_modes(self.ctx.engine_kernel, target) else {
            return Ok(None);
        };
        if modes.wants_mouse_tracking {
            return Ok(None);
        }
        // Alt-screen panes have no local scrollback: arrows under DECSET
        // 1007 (the default), else forward to the app.
        if modes.alt_screen {
            if modes.alt_scroll {
                self.send_wheel_as_arrows(target, delta).await?;
                return Ok(Some(false));
            }
            return Ok(None);
        }
        if self.scroll_pane_viewport(target, delta) {
            return Ok(Some(true));
        }
        // Local scroll did not move the viewport (already at the
        // edge, empty history, or an alt-screen replica whose
        // mode bits we missed). Forward so the inner app sees it.
        Ok(None)
    }

    /// Emit one arrow-key press per wheel notch — the alternate-scroll
    /// translation.
    async fn send_wheel_as_arrows(
        &mut self,
        target: &ResourceId,
        delta: isize,
    ) -> Result<(), AttachError> {
        let arrow = make_named_key(
            if delta < 0 {
                PhysicalKey::ArrowUp
            } else {
                PhysicalKey::ArrowDown
            },
            ModSet::empty(),
        );
        for _ in 0..delta.unsigned_abs() {
            self.send_terminal_input(target.clone(), InputEvent::Key(arrow.clone()), false)
                .await?;
        }
        Ok(())
    }

    /// Scroll `target`'s mirror by `delta`; true only if the viewport moved
    /// (libghostty reports `Ok` for no-op scrolls, which must forward).
    fn scroll_pane_viewport(&mut self, target: &ResourceId, delta: isize) -> bool {
        let scrolled = self
            .ctx
            .engine_kernel
            .published_engine_mut(target)
            .is_some_and(|replica| replica_scroll_moved(replica, delta));
        if !scrolled {
            return false;
        }
        if delta < 0
            && let Some(slot) = self.panes.get_mut(target)
        {
            slot.viewport_scrolled = true;
        }
        true
    }

    /// ADR-0058: a right press on a pane whose app does not track the mouse
    /// opens the pane menu at the (un-routed) pointer.
    fn open_pane_context_menu(&mut self, mouse: &MouseEvent, target: &ResourceId) -> bool {
        if !is_right_press(mouse) || !pane_ignores_mouse(self.ctx.engine_kernel, target) {
            return false;
        }
        let zoomed = self.ctx.zoomed.as_ref() == Some(target);
        let spec = crate::attach::context_menu::pane_menu(self.ctx.keybindings, zoomed);
        open_context_menu(
            self.ctx,
            spec,
            (quantize_cell(mouse.x), quantize_cell(mouse.y)),
        );
        true
    }

    /// Drag-to-copy (tmux): a left press on a pane whose app does not track
    /// the mouse, or any Ctrl-left press, starts a copy-mode selection;
    /// motion and release then route through the overlay stage.
    fn begin_drag_to_copy(&mut self, routed: &MouseEvent, target: &ResourceId) -> bool {
        let force_copy = routed.mods.contains(ModSet::CTRL);
        if !is_left_press(routed)
            || (!force_copy && !pane_ignores_mouse(self.ctx.engine_kernel, target))
        {
            return false;
        }
        let rect = focused_pane_rect(self.ctx, self.focused_resource.as_ref());
        let mouse_col = quantize_cell(routed.x).min(rect.w.saturating_sub(1));
        let mouse_row = quantize_cell(routed.y).min(rect.h.saturating_sub(1));
        let anchor = published_terminal(self.ctx.engine_kernel, target)
            .and_then(|terminal| {
                let cell = terminal
                    .grid_ref(Point::Viewport(PointCoordinate {
                        x: mouse_col,
                        y: u32::from(mouse_row),
                    }))
                    .ok()?;
                terminal
                    .point_from_grid_ref(&cell, PointSpace::Screen)
                    .ok()?
            })
            .map(|point| ScreenSelectionPoint {
                col: point.x,
                row: point.y,
            });
        let mut overlay =
            crate::render::overlay::CopyModeOverlay::new(mouse_row, mouse_col, rect.w, rect.h);
        if let Some(anchor) = anchor {
            overlay.set_mouse_anchor_screen(anchor);
        }
        self.ctx.overlays.push(Box::new(overlay));
        // Seed anchor + cursor from the (pane-local) press.
        let _ = self.ctx.overlays.handle_mouse(routed);
        true
    }

    /// ADR-0048: a left press on a divider grabs it and snaps the split to
    /// the pointer; other presses on a divider are dropped.
    fn grab_divider(
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

/// A primary-button press: the gesture the client's own chrome claims
/// (sidebar rows, status-bar tabs, divider grabs, drag-to-copy).
pub(super) fn is_left_press(mouse: &MouseEvent) -> bool {
    matches!(mouse.action, MouseAction::Press) && mouse.button == MouseButton::Left
}

/// A secondary-button press: the context-menu gesture (phux-wrnm,
/// ADR-0058).
pub(super) fn is_right_press(mouse: &MouseEvent) -> bool {
    matches!(mouse.action, MouseAction::Press) && mouse.button == MouseButton::Right
}

/// The three DEC private modes the wheel branch gates on, read in one
/// borrow of the pane's published mirror.
pub(super) struct PaneScrollModes {
    wants_mouse_tracking: bool,
    alt_screen: bool,
    alt_scroll: bool,
}

/// Read [`PaneScrollModes`] off `target`'s published mirror, or `None`
/// when the pane has no mirror yet.
pub(super) fn pane_scroll_modes(
    kernel: &crate::attach::pane_state::AttachKernel,
    target: &ResourceId,
) -> Option<PaneScrollModes> {
    let terminal = published_terminal(kernel, target)?;
    Some(PaneScrollModes {
        wants_mouse_tracking: terminal_wants_mouse_tracking(terminal),
        alt_screen: terminal_in_alt_screen(terminal),
        alt_scroll: terminal
            .mode(libghostty_vt::terminal::Mode::ALT_SCROLL)
            .unwrap_or(false),
    })
}

/// Whether `target`'s app has not enabled mouse tracking (then the client
/// may claim its buttons).
pub(super) fn pane_ignores_mouse(
    kernel: &crate::attach::pane_state::AttachKernel,
    target: &ResourceId,
) -> bool {
    published_terminal(kernel, target)
        .is_some_and(|terminal| !terminal_wants_mouse_tracking(terminal))
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
    let Some(ls) = ctx.workspace.active_window_mut() else {
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

/// Map a left press on the sidebar strip to the action it commits, through
/// the same `ResolvedAction` vocabulary as a keybinding: window rows
/// `select-window`, plugin section rows `focus-pane`, agent rows a local
/// `select-window` or a cross-session
/// `switch-session` (from the painted `targets`), session rows
/// `switch-session { name, host? }`, headers and overflow their management
/// views, `+ new` `new-window`, the chevron `toggle-sidebar`.
pub(in crate::attach::input_dispatch) fn sidebar_click_action(
    strip: crate::layout::Rect,
    targets: &crate::render::chrome::sidebar::SidebarTargets,
    x: u16,
    y: u16,
) -> Option<phux_config::keybind::ResolvedAction> {
    let action = match hit_test(strip, targets.counts, x, y)? {
        SidebarHit::Window(i) => return select_window_action(i),
        SidebarHit::NeedsYou(j) => return sidebar_agent_action(targets.needs_you.get(j)?),
        SidebarHit::Roster(j) => {
            return Some(sidebar_session_action(targets.roster.get(j)?.as_ref()?));
        }
        SidebarHit::Plugin(s, j) => {
            let (window, pane) = *targets.plugin.get(s)?.get(j)?;
            return focus_pane_action(window, pane);
        }
        SidebarHit::Sessions => "session-picker",
        SidebarHit::Fleet => "agent-fleet",
        SidebarHit::NewWindow => "new-window",
        SidebarHit::Collapse => "toggle-sidebar",
    };
    Some(bare_action(action))
}

/// Agent targets distinguish client-local focus from a cross-session attach.
fn sidebar_agent_action(
    target: &crate::render::chrome::sidebar::SidebarTarget,
) -> Option<phux_config::keybind::ResolvedAction> {
    use crate::render::chrome::sidebar::SidebarTarget;
    let (id, name, window, pane, resource) = match target {
        SidebarTarget::Window(index) => return select_window_action(*index),
        SidebarTarget::Session {
            id,
            name,
            window,
            pane,
            resource,
        } => (id, name, window, pane, resource),
    };
    if let Some(resource) = resource
        && let Some(action) = crate::render::chrome::sidebar::satellite_open_action(resource)
    {
        return Some(action);
    }
    let mut args = switch_session_args(name.clone(), *id);
    if let Some(resource) = resource {
        args.insert(
            "resource".to_owned(),
            toml::Value::String(phux_client::selector::format_terminal_id(resource)),
        );
    }
    // Layout-backed rows may also name window/pane. Graph-only rows omit
    // both so a click cannot fabricate a TUI index.
    if let Some(window) = *window {
        args.insert(
            "window".to_owned(),
            toml::Value::Integer(i64::try_from(window).ok()?),
        );
    }
    if let Some(pane) = *pane {
        args.insert(
            "pane".to_owned(),
            toml::Value::Integer(i64::try_from(pane).ok()?),
        );
    }
    Some(phux_config::keybind::ResolvedAction {
        action: "switch-session".to_owned(),
        args,
    })
}

/// Preserve host qualification even when two hosts use the same session name.
fn sidebar_session_action(
    target: &crate::render::chrome::sidebar::SessionRosterTarget,
) -> phux_config::keybind::ResolvedAction {
    if let Some(machine) = &target.switch_host {
        return switch_host_action(machine.clone(), target.name.clone());
    }
    let mut args = switch_session_args(target.name.clone(), target.id);
    if let Some(host) = &target.host {
        args.insert("host".to_owned(), toml::Value::String(host.clone()));
    }
    phux_config::keybind::ResolvedAction {
        action: "switch-session".to_owned(),
        args,
    }
}

/// Map a press on the status-bar row to its action: a tab commits
/// `select-window`, the `switch` chip the fleet, a navigation hint its
/// action. The hit test lives with the painter, so targets match the paint.
pub(in crate::attach::input_dispatch) fn bar_click_action(
    painter: Option<&crate::render::chrome::status_bar::StatusBarPainter>,
    x: u16,
) -> Option<phux_config::keybind::ResolvedAction> {
    match painter?.hit_at(x)? {
        phux_config::widget::CellHit::Window(index) => select_window_action(index),
        phux_config::widget::CellHit::Switch => Some(bare_action("agent-fleet")),
        phux_config::widget::CellHit::Action(action) => Some(bare_action(action)),
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

/// The active window's name, or an empty string when the workspace has no
/// windows yet. Used as the window menu's title.
fn active_window_name(ctx: &DispatchCtx<'_>) -> String {
    ctx.workspace
        .windows
        .get(ctx.workspace.active)
        .map_or_else(String::new, |w| w.name.clone())
}

/// Quantise an f64 pointer position (1-px-per-cell per SPEC §9.2.1) to an
/// outer-viewport cell, saturating into `u16` like the mouse hit-test.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "cell-quantised SGR/X10 input; saturate to keep malformed peers from breaking routing"
)]
pub(in crate::attach::input_dispatch) fn quantize_cell(p: f64) -> u16 {
    if p.is_nan() || p < 0.0 {
        0
    } else if p >= f64::from(u16::MAX) {
        u16::MAX
    } else {
        p as u16
    }
}
