//! Mouse routing for the dispatcher: the pointer stage of
//! [`EventEnv::dispatch_event`], chrome hit-tests (sidebar strip, status-bar
//! row, pane dividers), and the pane-level gestures the client claims
//! (click-to-focus, wheel, context menu, drag-to-copy).
//!
//! A child of `dispatch` so it can extend [`EventEnv`] and read the stage
//! types without widening their visibility.

use super::{
    AttachError, ContextMenu, DEFAULT_GROUP_ID, DispatchCtx, DividerGrab, DragGrab, EventEnv,
    FrameKind, InputEvent, ModSet, MouseAction, MouseButton, MouseEvent, PhysicalKey, Point,
    PointCoordinate, PointSpace, ResourceId, Scope, ScreenSelectionPoint, SidebarHit, StageOutcome,
    WindowStrip, actions, apply_focus_transition, chrome_drag, content_rect, encode_layout_or_log,
    focused_pane_rect, hit_test, layout_key, make_named_key, published_terminal,
    reanchor_predict_to_pane, replica_scroll_moved, scale_to_surface_pixels, switch_session_args,
    terminal_alt_scroll, terminal_in_alt_screen, terminal_wants_mouse_tracking, wheel_scroll_delta,
};

#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
impl<W: crate::attach::RenderSink> EventEnv<'_, '_, W> {
    /// phux-4li.6 / ADR-0048: `INPUT_MOUSE` routing + click-to-focus +
    /// divider drag-to-resize. The parser emits mouse coordinates in
    /// outer-viewport cells (treated as 1-px-per-cell f64 per SPEC
    /// §9.2.1); we hit-test against the multi-pane composition's
    /// `Rect`s. A press on a divider cell *grabs* the split that
    /// divider controls; button-motion while grabbed re-tunes the
    /// split's ratio so the divider tracks the cursor; release drops
    /// the grab. A click in a pane forwards the event (with pane-local
    /// coords) to that pane — so an inner TUI that turned mouse
    /// tracking on still receives every pointer event over its own
    /// cells (the divider cells are the only ones whose meaning the
    /// client claims).
    ///
    /// Every mouse event that reaches this stage is claimed by it.
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
        if let Some(outcome) = self.route_sidebar_click(mouse).await? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.route_status_bar_click(mouse).await? {
            return Ok(outcome);
        }
        // Hit-test against the SAME inset content rect the renderer tiles
        // into — status-bar row and sidebar columns folded off the outer
        // viewport. Routing against the full viewport instead disagrees with
        // what is painted: a click near a divider lands one row off (the
        // status bar) and, with a sidebar docked, one strip-width off in x,
        // so it focuses/forwards to the wrong pane. Clicks in the reserved
        // chrome miss every pane rect and become a Miss (dropped).
        let content = content_rect(self.ctx.viewport, self.ctx.bar, self.ctx.sidebar);
        // phux-jow6: hit-test against the RENDER layout, not the real
        // tiled tree. When a pane is zoomed (phux-x2hm) the render layout
        // is a single full-content leaf, so any click lands on the
        // visible zoomed pane instead of whichever hidden tiled pane sits
        // under the cursor. Compute the decision in a scope that drops the
        // borrowing `Cow` before the click-to-focus `active_window_mut()`
        // below needs the workspace mutably.
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
            // ADR-0048: a release ALWAYS ends the grab, wherever it lands —
            // the cursor may have left the handle mid-drag. A divider
            // publishes its final layout via SET_METADATA, the same path the
            // keyboard resize uses; a window drop publishes the new order.
            MouseAction::Release => {
                *self.ctx.drag = None;
                let commit = chrome_drag::release(self.ctx, &grab, mouse);
                if commit.broadcast {
                    self.broadcast_dragged_layout().await?;
                }
                tracing::debug!(?grab, "chrome drag: released");
                Ok(Some(StageOutcome::consumed(commit.layout_changed)))
            }
            // While something is grabbed, motion advances it and nothing
            // reaches a pane.
            MouseAction::Motion => Ok(Some(StageOutcome::consumed(chrome_drag::motion(
                self.ctx, &grab, mouse,
            )))),
            // phux-npb3 hardening (PR #142 review, recorded in ADR-0048):
            // anything else mid-drag (a second Press from a chorded button,
            // a wheel tick, a re-encoded press glitch) is consumed so it
            // cannot forward to a pane, move focus, or start a second grab.
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

    /// End a live chrome drag whose release will never arrive: the outer
    /// terminal lost focus, so the button went up somewhere this client
    /// cannot see. Without this the next press, anywhere, would be eaten as
    /// mid-drag noise. A divider keeps the ratio it reached and publishes
    /// it, exactly as a release would; a window drop has no position, so
    /// it is cancelled and the insertion marker must leave the strip.
    /// The focus event itself still reaches the pane.
    ///
    /// Returns whether the screen changed (a cancelled window drag still
    /// has a marker to erase).
    pub(super) async fn abandon_chrome_drag(&mut self) -> Result<bool, AttachError> {
        let Some(grab) = self.ctx.drag.take() else {
            return Ok(false);
        };
        if matches!(grab, DragGrab::Divider(_)) {
            self.broadcast_dragged_layout().await?;
            tracing::debug!(?grab, "chrome drag: abandoned on focus loss");
            return Ok(false);
        }
        tracing::debug!(?grab, "chrome drag: abandoned on focus loss");
        Ok(matches!(grab, DragGrab::Window(_)))
    }

    /// Broadcast the layout a finished divider or window drag produced via
    /// `SET_METADATA`, so other attached clients converge on it.
    async fn broadcast_dragged_layout(&mut self) -> Result<(), AttachError> {
        if self.ctx.layout_read_complete
            && let Some(session) = self.ctx.focused_session
            && let Some(bytes) = encode_layout_or_log(self.ctx.workspace)
        {
            let request_id = *self.ctx.next_request_id;
            *self.ctx.next_request_id = self.ctx.next_request_id.wrapping_add(1);
            self.conn
                .send(&FrameKind::SetMetadata {
                    request_id,
                    scope: Scope::Group(DEFAULT_GROUP_ID),
                    key: layout_key(session),
                    value: bytes,
                })
                .await?;
        }
        Ok(())
    }

    /// phux-fce4: the sidebar strip claims every pointer event over
    /// its own cells BEFORE pane routing — its rows are hit targets,
    /// not pane content. A left press resolves against the strip's
    /// row model (`sidebar::hit_test`) and dispatches the mapped
    /// action through the same `run_action` path a keybinding or
    /// palette row uses: a window block commits `select-window`, an
    /// agents-section row (phux-foz.9) `select-window` for the
    /// window holding that agent's pane, the `+ new` affordance
    /// `new-window`, `= menu` the command palette (the
    /// session/plugin menu), and the bottom-corner collapse chevron
    /// `toggle-sidebar`. Everything else over the strip (motion,
    /// non-left presses, headers, blank rows, the separator column)
    /// is consumed and dropped so it can never leak into a pane
    /// whose rect does not contain it anyway.
    ///
    /// `None` when the pointer is not over the strip.
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
            // phux-wrnm: a right press on a window block (or an
            // agents-section row, which resolves to the window
            // holding that agent) selects that window first —
            // acting on what you pointed at is the whole promise
            // of a context menu — and then opens the window menu
            // for it. Every other cell of the strip is session
            // chrome and gets the session menu, so a right-click
            // anywhere on the sidebar does something useful.
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

    /// phux-foz.12: the status-bar row is chrome, not pane content —
    /// `content_rect` already excludes it, so every pointer event
    /// here used to fall through to a Miss and get dropped. Claim
    /// the row explicitly instead: a left press on a window tab
    /// (resolved against the painter's cached strip, so the hit
    /// targets are exactly the cells on screen) dispatches
    /// `select-window { index }` through the same `run_action`
    /// path the sidebar affordances and keybindings use. phux-qtw8:
    /// the sidebar strip is full-height and claims its columns on
    /// THIS row too — but it hit-tests first (above), so by here the
    /// event is in the bar's own inset span and `window_hit_at`
    /// (which indexes off the origin it painted at) resolves it.
    /// Pane content is untouched — everything else on the row
    /// (non-tab cells, motion, wheel, non-left buttons) is consumed
    /// and dropped, matching the pre-claim behavior bit for bit.
    ///
    /// `None` when the pointer is not on the bar's row.
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
            // phux-wrnm: right press on a tab selects that window
            // (same as a left click) and opens its window menu;
            // elsewhere on the bar — the session name, the
            // widgets, the blank padding — the session menu. The
            // menu is clamped into the content rect, so a
            // bottom-docked bar opens it upward, over the panes.
            layout_changed = self.open_chrome_context_menu(hit, (cell_x, cell_y)).await?;
        }
        Ok(Some(StageOutcome::consumed(layout_changed)))
    }

    /// phux-wrnm: commit `window_row` (when the right press landed on a
    /// window target) and open the matching context menu at `anchor` —
    /// the window menu when it did, the session menu otherwise. Shared
    /// by the sidebar strip and the status-bar row.
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

    /// Route a pointer event that landed inside a pane's rect: click to
    /// focus, then the pane-level gestures the client claims (wheel,
    /// context menu, drag-to-copy), and finally the `INPUT_MOUSE`
    /// forward to the pane itself.
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
        // phux-npb3: a pane opted out via `set-pane mouse off`
        // receives no client-synthesized mouse at all — no
        // INPUT_MOUSE forward, no local wheel viewport scroll.
        // Click-to-focus above still applies: it is chrome-level
        // (the pane never sees it) and it is also the path that
        // makes the driver drop outer capture once the opted-out
        // pane is focused, restoring the host's raw handling.
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
        apply_focus_transition(
            &mut self.ctx.focus_history,
            self.focused_resource,
            target.clone(),
        );
        // Re-anchor predict to the clicked pane: drop the
        // old pane's queue AND reset the cursor + viewport
        // to the new pane, so a keystroke before the next
        // reconcile echoes at the right place rather than
        // the old pane's (mid-screen) coordinates (phux-7ry0).
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
        // Alt-screen panes have no client-local scrollback. Never
        // local-scroll them (phux-2vnl): a missed mouse-mode bit
        // used to feed `scroll_viewport` and either smear primary
        // history over the app or eat a silent no-op. Translate to
        // arrows when DECSET 1007 is on (libghostty default, same
        // as tmux/ghostty); otherwise forward the wheel so the
        // inner app can handle it. Apps opt out of arrows with
        // `?1007l` (phux-yyex).
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

    /// Scroll `target`'s local mirror by `delta`, returning `true` iff the
    /// viewport actually moved (the caller repaints). A successful
    /// `scroll_viewport` call is not enough: libghostty reports `Ok` at
    /// the live tail, on an empty history, and on the alt screen, and
    /// consuming those no-ops ate the wheel (phux-2vnl).
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

    /// phux-wrnm (ADR-0058): a right press on a pane whose app
    /// has NOT enabled mouse tracking opens the pane context
    /// menu at the pointer. The gate is the same boundary
    /// drag-to-copy respects: an inner program that asked for
    /// the mouse (vim, htop, a TUI with its own right-click
    /// menu) keeps every button, and the keyboard-bindable
    /// `context-menu` action is the way in for those panes.
    /// Click-to-focus above has already run, so the menu acts
    /// on the pane you pointed at, not the one you left.
    ///
    /// The menu is anchored in viewport cells, so this takes the
    /// un-routed event.
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

    /// Drag-to-copy (tmux convention): a left press on a pane
    /// whose app has NOT enabled mouse tracking starts a
    /// copy-mode selection anchored at the click. Holding Ctrl
    /// explicitly overrides an app's mouse tracking, matching the
    /// conventional terminal escape hatch for selecting output from a TUI.
    /// Motion and release then route through the overlay stage above —
    /// release copies to the host clipboard (OSC 52) and dismisses; a click
    /// without drag just dismisses. Without Ctrl, apps that DO track the
    /// mouse (vim, htop, Codex) keep receiving their events untouched.
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

    /// ADR-0048: a LEFT-button press on a divider starts a drag
    /// and immediately snaps the split to the press position (so
    /// a click-without-motion still nudges, matching the
    /// intuitive "grab here"). Scroll-wheel and right/middle
    /// presses encode as Press too, but landing on a 1-cell
    /// divider must not snap the split — those, and stray
    /// grab-less motions, are dropped (the divider gap has no
    /// pane to forward to).
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
        alt_scroll: terminal_alt_scroll(terminal),
    })
}

/// Whether `target`'s app has NOT enabled mouse tracking — the boundary
/// the pane context menu and drag-to-copy both respect: an inner program
/// that asked for the mouse (vim, htop, a TUI with its own right-click
/// menu) keeps every button.
pub(super) fn pane_ignores_mouse(
    kernel: &crate::attach::pane_state::AttachKernel,
    target: &ResourceId,
) -> bool {
    published_terminal(kernel, target)
        .is_some_and(|terminal| !terminal_wants_mouse_tracking(terminal))
}

/// Apply one drag step: re-tune the grabbed split so its divider tracks
/// `mouse`, returning `true` iff the layout changed (the caller repaints).
///
/// A pure mutation of the active window — no wire I/O (the `SET_METADATA`
/// broadcast happens once on release). Reuses [`actions::apply_divider_resize`]
/// so the drag, the keybind resize, and the persisted layout all run the
/// same `MIN_PANE_CELL` floor + `clamp_ratio` math. The pointer is
/// quantised to an outer-viewport cell exactly as the hit-test does.
/// `Ok(None)` from the resize (min-cell floor hit, or a stale grab whose
/// split the layout no longer has) leaves the layout untouched: the drag
/// stalls at the floor rather than collapsing a pane.
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

/// phux-fce4: whether an outer-viewport cell lies within the sidebar
/// strip's rect (separator column included — the strip consumes it even
/// though it is not a hit target).
const fn strip_contains(rect: crate::layout::Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.w)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.h)
}

/// phux-fce4: map a left press on the sidebar strip to the action it
/// commits, or `None` when it lands on a header, blank row, or the
/// separator.
///
/// The mapping goes through [`ResolvedAction`] so a sidebar click runs
/// exactly what a keybinding, palette row, or overlay commit would — one
/// dispatch path, no bespoke click semantics:
///
/// * a nested window row commits `select-window { index }`;
/// * an agent row commits `select-window` when the
///   agent is in this session, and `switch-session { name, resource }`
///   (plus window/pane only when a TUI layout named them) when it is in
///   another one — the row resolves through `targets`, which
///   carries the NAME the frame was painted with rather than re-deriving it
///   from a live model;
/// * a session name or host row commits `switch-session { name, host? }`;
/// * Agents overflow opens `agent-fleet`; Sessions overflow opens `session-picker`;
/// * `+ new` commits `new-window` (the strip lists windows, so its create
///   affordance creates one);
/// * the Agents / Sessions headings open their complete management views;
/// * the collapse chevron in the bottom corner (phux-foz.9) commits
///   `toggle-sidebar`.
pub(in crate::attach::input_dispatch) fn sidebar_click_action(
    strip: crate::layout::Rect,
    targets: &crate::render::chrome::sidebar::SidebarTargets,
    x: u16,
    y: u16,
) -> Option<phux_config::keybind::ResolvedAction> {
    let (action, args) = match hit_test(strip, targets.counts, x, y)? {
        SidebarHit::Window(i) => return sidebar_window_action(i),
        SidebarHit::NeedsYou(j) => return sidebar_agent_action(targets.needs_you.get(j)?),
        SidebarHit::Roster(j) => {
            return Some(sidebar_session_action(targets.roster.get(j)?.as_ref()?));
        }
        SidebarHit::Sessions => ("session-picker", std::collections::BTreeMap::new()),
        SidebarHit::Fleet => ("agent-fleet", std::collections::BTreeMap::new()),
        SidebarHit::NewWindow => ("new-window", std::collections::BTreeMap::new()),
        SidebarHit::Collapse => ("toggle-sidebar", std::collections::BTreeMap::new()),
    };
    Some(phux_config::keybind::ResolvedAction {
        action: action.to_owned(),
        args,
    })
}

/// Build a window selection through the registry's index argument.
fn sidebar_window_action(index: usize) -> Option<phux_config::keybind::ResolvedAction> {
    Some(phux_config::keybind::ResolvedAction {
        action: "select-window".to_owned(),
        args: std::collections::BTreeMap::from([(
            "index".to_owned(),
            toml::Value::Integer(i64::try_from(index).ok()?),
        )]),
    })
}

/// Agent targets distinguish client-local focus from a cross-session attach.
fn sidebar_agent_action(
    target: &crate::render::chrome::sidebar::SidebarTarget,
) -> Option<phux_config::keybind::ResolvedAction> {
    use crate::render::chrome::sidebar::SidebarTarget;
    let (id, name, window, pane, resource) = match target {
        SidebarTarget::Window(index) => return sidebar_window_action(*index),
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
    // both so a click cannot fabricate a TUI index (phux-ah84).
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
    let mut args = switch_session_args(target.name.clone(), target.id);
    if let Some(host) = &target.host {
        args.insert("host".to_owned(), toml::Value::String(host.clone()));
    }
    phux_config::keybind::ResolvedAction {
        action: "switch-session".to_owned(),
        args,
    }
}

/// phux-foz.12: map a left press on the status-bar row to the action it
/// commits, or `None` when it lands on a non-tab cell (separator, another
/// widget, blank padding) or no painter/strip is available. Named navigation
/// cells dispatch their argument-free action through this same path.
///
/// Same shape as [`sidebar_click_action`]: the mapping goes through
/// [`phux_config::keybind::ResolvedAction`] so a tab click runs exactly
/// what a keybinding, palette row, or sidebar click would — one dispatch
/// path, no bespoke click semantics. A window tab commits
/// `select-window { index }`; the hit test itself lives with the painter
/// ([`crate::render::chrome::status_bar::StatusBarPainter::hit_at`])
/// so paint and click targets derive from the same composed strip.
pub(in crate::attach::input_dispatch) fn bar_click_action(
    painter: Option<&crate::render::chrome::status_bar::StatusBarPainter>,
    x: u16,
) -> Option<phux_config::keybind::ResolvedAction> {
    match painter?.hit_at(x)? {
        phux_config::widget::CellHit::Window(index) => {
            let mut args = std::collections::BTreeMap::new();
            args.insert(
                "index".to_owned(),
                toml::Value::Integer(i64::try_from(index).ok()?),
            );
            Some(phux_config::keybind::ResolvedAction {
                action: "select-window".to_owned(),
                args,
            })
        }
        // The `switch` chip opens the fleet dashboard — the same overlay
        // `prefix A` opens, through the same dispatch path. It is the
        // right target for a pointer because it is the *only* switcher
        // that answers all three questions at once (which sessions, which
        // windows, which agent needs me), and on the narrow terminal
        // where the chip is shown that is the whole point.
        phux_config::widget::CellHit::Switch => Some(phux_config::keybind::ResolvedAction {
            action: "agent-fleet".to_owned(),
            args: std::collections::BTreeMap::new(),
        }),
        phux_config::widget::CellHit::Action(action) => {
            Some(phux_config::keybind::ResolvedAction {
                action: action.to_owned(),
                args: std::collections::BTreeMap::new(),
            })
        }
    }
}

/// phux-wrnm: push `spec` as a context menu anchored at the viewport cell
/// `anchor` (ADR-0058).
///
/// The menu is clamped inside the pane content rect — the same rect the
/// panes tile into and centered modals are placed against — so it can
/// never occlude the sidebar strip or the status-bar row, including when
/// the click that opened it landed on that chrome.
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
