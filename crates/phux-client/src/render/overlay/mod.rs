//! Overlay layer — modals, help, command palette.
//!
//! An overlay is a chrome-layer widget that takes over the outer terminal
//! while it's active: input is captured (no keystrokes reach the focused
//! pane's stdin) and pane stdout flushing is paused (per ADR-0020 §Decision
//! invariant 5). Pane libghostty mirrors keep consuming server VT bytes —
//! we only pause the *outbound* flush so the modal doesn't get trampled by
//! a `TERMINAL_OUTPUT` repaint. On dismiss, the driver triggers a full
//! repaint to restore pane content.
//!
//! [`OverlayState`] carries a *stack* of overlays. The top of the stack
//! captures input ([`RenderOverlay::handle_key`]); rendering walks the
//! stack bottom-up so stacked overlays compose (e.g. a command palette
//! painted on top of an open help modal). A single active overlay is the
//! one-element case — the common path is unchanged from a UX standpoint.
//!
//! Submodules:
//! - [`help`] — keybindings reference modal (phux-5ke.4)
//! - [`prompt`] — single-line text-input modal (phux-ahv.1)
//! - [`widgets`] — reusable themed primitives ([`Modal`], [`KeyChordTable`])
//!
//! [`Modal`]: widgets::Modal
//! [`KeyChordTable`]: widgets::KeyChordTable

use std::io::{self, Write};

use phux_protocol::input::key::KeyEvent;
use phux_protocol::input::mouse::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

pub mod copy_mode;
pub mod help;
pub mod menu;
pub mod prompt;
pub mod select_list;
pub mod selection;
pub mod toast;
pub mod which_key;
pub mod widgets;

pub use copy_mode::CopyModeOverlay;
pub use help::HelpOverlay;
pub use menu::{ContextMenu, MenuRow};
pub use prompt::PromptOverlay;
pub use select_list::{SelectItem, SelectList};
// The shared copy-mode selection contract (ADR-0045). This module is the single
// owner; both the selection UX (`copy_mode`) and the renderer (`attach::render`)
// import these types from here, so the highlight geometry and the copy path
// cannot disagree about what a selection covers.
pub use selection::{CopyRequest, SelectionGrab, SelectionMode, SelectionRect};
pub use toast::ToastOverlay;
pub use which_key::WhichKeyOverlay;

/// Test double: a [`RenderOverlay`] that records every key handed to it and
/// never dismisses, so a test can assert exactly which keystrokes reached
/// the overlay. Lives here (not in `attach/`) because implementing
/// `RenderOverlay::render` names ratatui types, which the boundary guard
/// confines to `render/`. Used by the `attach::input_dispatch` overlay-input
/// routing regression test.
#[cfg(test)]
pub(crate) struct RecordingOverlay {
    pub(crate) keys: std::rc::Rc<std::cell::RefCell<Vec<KeyEvent>>>,
}

#[cfg(test)]
impl RenderOverlay for RecordingOverlay {
    fn render(&self, _area: Rect, _buf: &mut Buffer) {}
    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        self.keys.borrow_mut().push(key.clone());
        OverlayCommand::Stay
    }
}

/// A chrome-layer overlay rendered above pane interiors.
///
/// Implementors paint into a ratatui [`Buffer`] sized to the outer
/// viewport. [`handle_key`] receives structured [`KeyEvent`]s from the
/// driver — phux uses libghostty/protocol input atoms per ADR-0006 and
/// ADR-0008, NOT crossterm's event types, even though the rendering
/// toolkit (ratatui) is crossterm-adjacent. The overlay is responsible
/// for deciding when it's done; the driver inspects the returned
/// [`OverlayCommand`].
///
/// [`handle_key`]: RenderOverlay::handle_key
pub trait RenderOverlay {
    /// Paint into `buf` covering `area` (typically the full outer
    /// viewport). Cells the overlay does not write to are left as the
    /// `Buffer`'s default (blank black-on-default).
    fn render(&self, area: Rect, buf: &mut Buffer);

    /// React to a key event. Return [`OverlayCommand::Dismiss`] to close
    /// the overlay; [`OverlayCommand::Stay`] to keep it open and consume
    /// the key.
    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand;

    /// React to a mouse event while this overlay is active. Most modal
    /// overlays ignore pointer input; copy-mode consumes wheel events to
    /// scroll the focused pane's client-local viewport.
    fn handle_mouse(&mut self, _mouse: &MouseEvent) -> OverlayCommand {
        OverlayCommand::Stay
    }

    /// The painted region of this overlay inside `area`, or `None` if it
    /// paints the whole viewport.
    ///
    /// A bounded overlay (every modal: help, prompt, command palette,
    /// pickers) returns the centered `Rect` it draws into. The driver uses
    /// it to **float** the modal: it repaints the live panes as the base
    /// frame and then emits only this region on top, so the panes stay
    /// visible around the box instead of vanishing behind a full-screen
    /// clear (the "overlay overflows the whole screen" bug). Default `None`
    /// keeps the legacy full-screen behaviour for any overlay that genuinely
    /// owns the entire viewport.
    fn bounds(&self, _area: Rect) -> Option<Rect> {
        None
    }

    /// The active copy-mode selection (pane-local cells), or `None`.
    ///
    /// `None` for every modal overlay (the default) — they paint their own
    /// surface and the driver clears the screen for them. Copy-mode returns
    /// `Some`: it is *not* a modal overlay but a selection highlight over the
    /// live pane, so the driver repaints the focused pane with these cells
    /// reverse-videoed (via [`crate::attach::render::TerminalRenderer::set_selection`])
    /// instead of clearing the screen. Nothing on screen swaps; only the
    /// selected cells invert.
    fn copy_selection(&self) -> Option<SelectionRect> {
        None
    }

    /// `true` for a display-only overlay that must never capture input
    /// (phux-foz.2: the which-key popup). The dispatcher checks this
    /// BEFORE overlay routing: instead of feeding keys to the overlay it
    /// dismisses it and processes the event exactly as if the overlay
    /// were not there, so a passthrough overlay can never eat or delay a
    /// chord. Default `false` — every modal overlay captures input.
    fn is_input_passthrough(&self) -> bool {
        false
    }

    /// Whether Escape has special prefix-cancellation meaning for this
    /// passthrough overlay. Only which-key needs this; ordinary notices pass
    /// Escape through like every other intended input.
    fn passthrough_escape_cancels_prefix(&self) -> bool {
        false
    }

    /// phux-wrnm: `true` for an overlay that hover-tracks the pointer with
    /// no button held (the context menu). The driver upgrades the outer
    /// terminal's mouse reporting from button-event (`?1002h`) to
    /// any-motion (`?1003h`) while such an overlay is on the stack, and
    /// drops back the moment it closes — hover traffic is worth the bytes
    /// only while something on screen consumes it (ADR-0048's rationale
    /// for not enabling `?1003h` by default stands otherwise).
    fn wants_pointer_hover(&self) -> bool {
        false
    }

    /// phux-fsb: whether this overlay's geometry stays valid across a
    /// viewport resize.
    ///
    /// Every centered overlay lays itself out from the `area` handed to
    /// [`RenderOverlay::render`] and [`RenderOverlay::bounds`] on each
    /// paint, so it reflows into the new viewport for free — `true`, the
    /// default. The context menu is the exception: it pins its box to the
    /// pointer cell at construction, against the content rect that existed
    /// *then*. After a resize that box can be off screen entirely while the
    /// overlay stays active, capturing every keystroke and committing its
    /// selected row on Enter with nothing painted to show what it is. Such
    /// an overlay returns `false` and the driver drops it on SIGWINCH
    /// ([`OverlayState::dismiss_stale_on_resize`]).
    fn survives_resize(&self) -> bool {
        true
    }

    /// phux-d26y: re-derive geometry from the focused pane's new size after
    /// a viewport resize. `pane_cols` / `pane_rows` are the focused pane's
    /// dimensions in the *new* layout.
    ///
    /// The companion to [`RenderOverlay::survives_resize`]: an overlay that
    /// survives a resize still has to be *told* about it if it cached
    /// anything derived from the old size. Copy-mode is the one that does —
    /// it clamps its cursor, quantizes mouse positions, sizes a page scroll,
    /// and picks Line mode's right edge from pane dimensions captured at
    /// construction. Everything else recomputes from the render `area` and
    /// ignores this (the default no-op).
    fn on_viewport_resize(&mut self, _pane_cols: u16, _pane_rows: u16) {}

    /// phux-foz.7: offer this overlay a freshly rebuilt row set tagged
    /// `key`. A *live* overlay whose data projects shared client state —
    /// the agent-fleet dashboard — replaces its rows in place (preserving
    /// query/selection) and returns `true` so the driver repaints it; every
    /// other overlay ignores the offer (default `false`). This is push, not
    /// poll: the driver calls it only when a server frame actually changed
    /// the projected state.
    fn refresh_items(&mut self, _key: &str, _items: &[SelectItem]) -> bool {
        false
    }
}

/// What an overlay wants the driver to do after [`RenderOverlay::handle_key`].
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayCommand {
    /// Keep the overlay active; the key was consumed.
    Stay,
    /// Close the overlay. The driver triggers a full repaint to restore
    /// pane content on the next loop iteration.
    Dismiss,
    /// Close the overlay and run this action in the dispatcher — e.g. a
    /// committed rename prompt returning `rename-window { name }`. The
    /// dispatcher feeds it through the normal `run_action` path.
    Commit(phux_config::keybind::ResolvedAction),
    /// Close the overlay and copy the current selection to the host clipboard
    /// (copy-mode Enter).
    ///
    /// Selection is a client-local projection over the focused pane's own
    /// engine ([ADR-0030]); the dispatcher resolves the [`CopyRequest`]
    /// against that engine and emits OSC 52 — nothing goes on the wire.
    ///
    /// [ADR-0030]: ../../../../ADR/0030-engine-delegated-wire-and-projection-consumers.md
    Copy(CopyRequest),
    /// Keep the overlay active and scroll the focused pane's client-local
    /// viewport by `delta` rows (negative means up into scrollback).
    ScrollViewport(isize),
}

/// What [`OverlayState::handle_key`] hands back to the dispatcher.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum OverlayOutcome {
    /// Nothing to do (key consumed, overlay stayed or dismissed).
    #[default]
    None,
    /// The overlay committed; run this action.
    RunAction(phux_config::keybind::ResolvedAction),
    /// Copy the resolved selection to the host clipboard (copy-mode).
    Copy(CopyRequest),
    /// Scroll the focused pane's local terminal viewport while the overlay
    /// remains active.
    ScrollViewport(isize),
}

/// Stacked overlay state.
///
/// The top of the stack captures input; rendering walks the stack
/// bottom-up so stacked overlays compose (palette painted on top of
/// help). A single active overlay is the one-element case.
#[derive(Default)]
pub struct OverlayState {
    /// Bottom-to-top overlay stack. The last element is the top — it
    /// receives input and is painted last (on top of the others).
    stack: Vec<Box<dyn RenderOverlay>>,
}

impl std::fmt::Debug for OverlayState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayState")
            .field("depth", &self.stack.len())
            .finish()
    }
}

impl OverlayState {
    /// Empty state — no overlay active.
    #[must_use]
    pub const fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// `true` when at least one overlay is active (capturing input +
    /// pausing pane stdout). The driver loop reads this *only* via this
    /// accessor — it must not see ratatui types directly (CI grep guard
    /// enforces).
    #[must_use]
    pub const fn is_active(&self) -> bool {
        !self.stack.is_empty()
    }

    /// The active (top) overlay's copy-mode selection, if it is copy-mode.
    ///
    /// `Some` only when copy-mode is the top overlay; the driver uses it to
    /// repaint the focused pane with the selection reverse-videoed rather than
    /// clearing the screen for a modal overlay. `None` for modal overlays or
    /// no overlay.
    #[must_use]
    pub fn copy_selection(&self) -> Option<SelectionRect> {
        self.stack.last().and_then(|o| o.copy_selection())
    }

    /// Number of overlays currently stacked (0 when inactive).
    #[must_use]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// `true` when the top overlay is input-passthrough (phux-foz.2:
    /// the which-key popup). The dispatcher consults this before overlay
    /// routing so a passthrough overlay is dismissed by — and never
    /// consumes — the next input event.
    #[must_use]
    pub fn top_is_passthrough(&self) -> bool {
        self.stack.last().is_some_and(|o| o.is_input_passthrough())
    }

    #[must_use]
    /// Whether the active passthrough overlay gives Escape the which-key
    /// prefix-cancellation behavior.
    pub fn passthrough_escape_cancels_prefix(&self) -> bool {
        self.stack
            .last()
            .is_some_and(|o| o.passthrough_escape_cancels_prefix())
    }

    /// phux-wrnm: `true` when any stacked overlay hover-tracks the pointer
    /// (see [`RenderOverlay::wants_pointer_hover`]). The driver reconciles
    /// the outer terminal's motion reporting against this each loop
    /// iteration, exactly the way it reconciles capture against focus.
    #[must_use]
    pub fn wants_pointer_hover(&self) -> bool {
        self.stack.iter().any(|o| o.wants_pointer_hover())
    }

    /// phux-fsb: drop every overlay whose geometry a viewport resize
    /// invalidated ([`RenderOverlay::survives_resize`]), returning `true`
    /// when the stack changed so the caller repaints.
    ///
    /// Called from the driver's SIGWINCH arm before it repaints. Dropping
    /// rather than reflowing is the honest move for a pinned overlay: its
    /// box was anchored to a pointer position on a screen that no longer
    /// exists, and re-placing it would put a menu somewhere the user never
    /// clicked. Native menus close on resize for the same reason.
    ///
    /// The whole stack is swept, not just the top, so a pinned overlay can
    /// never be stranded underneath a survivor.
    pub fn dismiss_stale_on_resize(&mut self) -> bool {
        let before = self.stack.len();
        self.stack.retain(|overlay| overlay.survives_resize());
        self.stack.len() != before
    }

    /// phux-d26y: hand every surviving overlay the focused pane's new size
    /// ([`RenderOverlay::on_viewport_resize`]).
    ///
    /// Runs *after* [`Self::dismiss_stale_on_resize`] on the same SIGWINCH
    /// edge, so the overlays that are about to be dropped are never asked to
    /// re-derive geometry they will not use. Most overlays ignore it; the
    /// one that does not is copy-mode, whose clamp is otherwise still
    /// talking about the pane size that existed when it opened.
    pub fn on_viewport_resize(&mut self, pane_cols: u16, pane_rows: u16) {
        for overlay in &mut self.stack {
            overlay.on_viewport_resize(pane_cols, pane_rows);
        }
    }

    /// Push `overlay` onto the top of the stack. It becomes the input
    /// target and is painted last (above any overlays beneath it).
    pub fn push(&mut self, overlay: Box<dyn RenderOverlay>) {
        self.stack.push(overlay);
    }

    /// Dismiss (pop) the top overlay, revealing whatever was beneath it.
    /// No-op when the stack is empty.
    pub fn dismiss(&mut self) {
        self.stack.pop();
    }

    /// Dispatch a key event to the top overlay. Auto-dismisses (pops) on
    /// [`OverlayCommand::Dismiss`], [`OverlayCommand::Commit`], and
    /// [`OverlayCommand::Copy`]; `Commit` also returns the action and `Copy`
    /// the selection for the dispatcher to handle. No-op (returns
    /// [`OverlayOutcome::None`]) when no overlay is active.
    pub fn handle_key(&mut self, key: &KeyEvent) -> OverlayOutcome {
        let Some(top) = self.stack.last_mut() else {
            return OverlayOutcome::None;
        };
        match top.handle_key(key) {
            OverlayCommand::Stay => OverlayOutcome::None,
            OverlayCommand::Dismiss => {
                self.dismiss();
                OverlayOutcome::None
            }
            OverlayCommand::Commit(action) => {
                self.dismiss();
                OverlayOutcome::RunAction(action)
            }
            OverlayCommand::Copy(req) => {
                // Copy-mode Enter: dismiss the overlay (tmux-style copy-and-exit)
                // and hand the dispatcher the selection to resolve locally.
                self.dismiss();
                OverlayOutcome::Copy(req)
            }
            OverlayCommand::ScrollViewport(delta) => OverlayOutcome::ScrollViewport(delta),
        }
    }

    /// phux-foz.7: hand a freshly rebuilt row set tagged `key` to the
    /// stacked overlays, top-down. The first overlay that accepts it (a
    /// live [`SelectList`] carrying the matching key — the agent-fleet
    /// dashboard) replaces its rows and stops the walk; `true` means some
    /// overlay refreshed and the caller should repaint the overlay layer.
    /// `false` (no live overlay in the stack) costs nothing downstream.
    pub fn refresh_items(&mut self, key: &str, items: &[SelectItem]) -> bool {
        self.stack
            .iter_mut()
            .rev()
            .any(|overlay| overlay.refresh_items(key, items))
    }

    /// Dispatch a mouse event to the top overlay. The overlay stays active;
    /// scroll requests are handed back to the dispatcher for local terminal
    /// viewport mutation.
    pub fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayOutcome {
        let Some(top) = self.stack.last_mut() else {
            return OverlayOutcome::None;
        };
        match top.handle_mouse(mouse) {
            OverlayCommand::ScrollViewport(delta) => OverlayOutcome::ScrollViewport(delta),
            OverlayCommand::Dismiss => {
                self.dismiss();
                OverlayOutcome::None
            }
            OverlayCommand::Commit(action) => {
                self.dismiss();
                OverlayOutcome::RunAction(action)
            }
            OverlayCommand::Copy(req) => {
                self.dismiss();
                OverlayOutcome::Copy(req)
            }
            OverlayCommand::Stay => OverlayOutcome::None,
        }
    }

    /// The bounding `Rect` to float the active overlay stack within, or
    /// `None` if any stacked overlay paints the whole viewport.
    ///
    /// Returns the union of every stacked overlay's [`RenderOverlay::bounds`],
    /// each centered inside `content` — the pane content rect (the viewport
    /// minus the sidebar strip and status-bar row), NOT the raw viewport
    /// (phux-foz.14). Centering against `content` keeps a modal's box off the
    /// sidebar columns, so it never occludes the chrome the floating-modal
    /// base-frame repaint (phux-foz.10) works to preserve.
    ///
    /// When `Some`, the driver paints the live panes as a base frame and
    /// emits only this region on top (a true floating modal); when `None`,
    /// it falls back to the full-screen clear+paint. Copy-mode is handled
    /// earlier (via [`Self::copy_selection`]) and never reaches here.
    #[must_use]
    pub fn active_bounds(&self, content: Rect) -> Option<Rect> {
        if self.stack.is_empty() {
            return None;
        }
        let mut union: Option<Rect> = None;
        for overlay in &self.stack {
            // Any full-screen overlay forces the whole-viewport path.
            let b = overlay.bounds(content)?;
            union = Some(union.map_or(b, |u| u.union(b)));
        }
        union
    }

    /// Paint the overlay stack into a fresh full-viewport buffer and emit
    /// it to `out` as VT bytes. No-op when no overlay is active.
    ///
    /// Overlays paint bottom-up into one shared buffer (top last), so a
    /// stacked overlay composes over the ones beneath it. The paint is a
    /// from-scratch render, not a diff: callers should clear-screen
    /// before invoking so leftover pane content doesn't bleed through the
    /// overlays' blank cells.
    pub fn paint(&self, out: &mut impl Write, viewport_dims: (u16, u16)) -> io::Result<()> {
        if self.stack.is_empty() {
            return Ok(());
        }
        let area = Rect::new(0, 0, viewport_dims.0, viewport_dims.1);
        let mut buf = Buffer::empty(area);
        for overlay in &self.stack {
            overlay.render(area, &mut buf);
        }
        emit_buffer(out, &buf)
    }

    /// Paint the overlay stack but emit **only** the cells inside `clip`
    /// (plus a one-cell drop shadow below + right of it).
    ///
    /// This is the floating-modal path: the caller has already painted the
    /// live panes as the base frame, so emitting only the modal's bounded
    /// region leaves the panes visible around it. Cells outside `clip` (and
    /// the shadow) are never written, so nothing erases the panes. The
    /// `shadow` color gives the box depth over the panes; pass `Color::Reset`
    /// to disable it. No-op when inactive.
    ///
    /// Overlays render against `content` — the pane content rect (viewport
    /// minus the sidebar strip and status-bar row), so a centered modal lands
    /// inside the pane region rather than over the chrome (phux-foz.14). The
    /// buffer stays full-viewport sized (`viewport_dims`) so `clip` and the
    /// drop shadow keep absolute screen coordinates.
    pub fn paint_clipped(
        &self,
        out: &mut impl Write,
        viewport_dims: (u16, u16),
        content: Rect,
        clip: Rect,
        shadow: Color,
    ) -> io::Result<()> {
        if self.stack.is_empty() {
            return Ok(());
        }
        let area = Rect::new(0, 0, viewport_dims.0, viewport_dims.1);
        let mut buf = Buffer::empty(area);
        for overlay in &self.stack {
            overlay.render(content, &mut buf);
        }
        emit_buffer_clipped(out, &mut buf, clip.intersection(area), shadow)
    }
}

/// Emit a ratatui [`Buffer`] to `out` as VT cursor + SGR + glyph bytes.
///
/// Walks row by row; emits CUP at column 0 of each row, then writes each
/// cell's symbol with an SGR delta. This is a deliberately simple
/// renderer (one CUP per row, full SGR re-emit per cell-with-style) —
/// overlays repaint on input, not on every frame, so we trade per-cell
/// efficiency for code that's obvious to audit. The chrome submodule
/// (status bar + dividers) may grow a shared writer later; for now the
/// overlay path stays self-contained.
fn emit_buffer(out: &mut impl Write, buf: &Buffer) -> io::Result<()> {
    let area = buf.area;
    // Hide cursor for the duration of the modal paint.
    out.write_all(b"\x1b[?25l")?;
    for row in 0..area.height {
        // CUP to start of row (1-based).
        write!(out, "\x1b[{};{}H", row + 1, 1)?;
        // Reset SGR so the previous row's tail style can't leak.
        out.write_all(b"\x1b[0m")?;
        let mut prev_styled = false;
        for col in 0..area.width {
            let cell = &buf[(area.x + col, area.y + row)];
            crate::render::sgr::emit_cell_sgr(out, cell, &mut prev_styled)?;
            let sym = cell.symbol();
            if sym.is_empty() {
                out.write_all(b" ")?;
            } else {
                out.write_all(sym.as_bytes())?;
            }
        }
        if prev_styled {
            out.write_all(b"\x1b[0m")?;
        }
    }
    // Park the cursor at (1,1) — overlay-active state implies no pane
    // cursor visible. Stays hidden until the overlay dismisses and the
    // pane re-paint emits its own DECTCEM.
    out.write_all(b"\x1b[1;1H")?;
    out.flush()
}

/// Emit only the cells of `buf` that fall inside `clip` (a floating modal's
/// bounded region), plus a one-cell drop shadow below + right of it, leaving
/// everything else on screen untouched.
///
/// The caller paints the live panes first; this writes the modal box on top
/// without erasing the panes around it. Each row CUPs to its own left edge so
/// only the box (and shadow band) cells are written. The shadow band is
/// painted into `buf` as `shadow`-bg spaces; the two outer corners (top-right
/// of the box, bottom-left of the shadow) are deliberately skipped so the L
/// reads as a shadow rather than a full rectangle. Pass `Color::Reset` to
/// disable the shadow.
fn emit_buffer_clipped(
    out: &mut impl Write,
    buf: &mut Buffer,
    clip: Rect,
    shadow: Color,
) -> io::Result<()> {
    if clip.width == 0 || clip.height == 0 {
        return Ok(());
    }
    let vp_w = buf.area.width;
    let vp_h = buf.area.height;
    let bx = clip.x;
    let by = clip.y;
    let rx = clip.x + clip.width; // box right edge (exclusive) = shadow column
    let ry = clip.y + clip.height; // box bottom edge (exclusive) = shadow row
    // The shadow bands exist only where there's a pane cell to cast onto.
    let shadow_col = !matches!(shadow, Color::Reset) && rx < vp_w;
    let shadow_row = !matches!(shadow, Color::Reset) && ry < vp_h;
    let shadow_style = ratatui::style::Style::default().bg(shadow);
    if shadow_col {
        // Right band: beside the box's lower rows + the bottom-right corner.
        for y in (by + 1)..=ry.min(vp_h - 1) {
            if let Some(cell) = buf.cell_mut((rx, y)) {
                cell.set_symbol(" ");
                cell.set_style(shadow_style);
            }
        }
    }
    if shadow_row {
        // Bottom band: beneath the box, starting one cell in (skip the corner).
        for x in (bx + 1)..=rx.min(vp_w - 1) {
            if let Some(cell) = buf.cell_mut((x, ry)) {
                cell.set_symbol(" ");
                cell.set_style(shadow_style);
            }
        }
    }

    out.write_all(b"\x1b[?25l")?;
    // Box rows. The top row omits the right shadow cell (no shadow above the
    // box); lower rows extend one cell right to include the shadow column.
    for row in by..ry {
        let end = if shadow_col && row > by { rx + 1 } else { rx };
        emit_row_span(out, buf, row, bx, end)?;
    }
    // Bottom shadow row, skipping the bottom-left corner (start at bx + 1).
    if shadow_row {
        let end = if shadow_col { rx + 1 } else { rx };
        emit_row_span(out, buf, ry, bx + 1, end)?;
    }
    // Park the (hidden) cursor at the modal origin; the next pane repaint on
    // dismiss emits its own DECTCEM.
    write!(out, "\x1b[{};{}H", by + 1, bx + 1)?;
    out.flush()
}

/// Emit cells `[start_col, end_col)` of `row` from `buf` with a leading CUP to
/// the row's start column and a per-cell SGR delta. Shared by the box rows and
/// the shadow band in [`emit_buffer_clipped`].
fn emit_row_span(
    out: &mut impl Write,
    buf: &Buffer,
    row: u16,
    start_col: u16,
    end_col: u16,
) -> io::Result<()> {
    write!(out, "\x1b[{};{}H", row + 1, start_col + 1)?;
    out.write_all(b"\x1b[0m")?;
    let mut prev_styled = false;
    for col in start_col..end_col {
        let cell = &buf[(col, row)];
        crate::render::sgr::emit_cell_sgr(out, cell, &mut prev_styled)?;
        let sym = cell.symbol();
        if sym.is_empty() {
            out.write_all(b" ")?;
        } else {
            out.write_all(sym.as_bytes())?;
        }
    }
    if prev_styled {
        out.write_all(b"\x1b[0m")?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_protocol::input::key::{KeyAction, ModSet, PhysicalKey};

    fn key(k: PhysicalKey) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key: k,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        }
    }

    struct EscDismiss;
    impl RenderOverlay for EscDismiss {
        fn render(&self, _area: Rect, _buf: &mut Buffer) {}
        fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
            if key.key == PhysicalKey::Escape {
                OverlayCommand::Dismiss
            } else {
                OverlayCommand::Stay
            }
        }
    }

    #[test]
    fn state_starts_inactive() {
        let s = OverlayState::new();
        assert!(!s.is_active());
    }

    #[test]
    fn push_then_dismiss_round_trip() {
        let mut s = OverlayState::new();
        s.push(Box::new(EscDismiss));
        assert!(s.is_active());
        s.dismiss();
        assert!(!s.is_active());
    }

    #[test]
    fn handle_key_auto_dismisses_on_esc() {
        let mut s = OverlayState::new();
        s.push(Box::new(EscDismiss));
        s.handle_key(&key(PhysicalKey::Escape));
        assert!(!s.is_active(), "Esc should dismiss");
    }

    #[test]
    fn handle_key_stays_on_other_keys() {
        let mut s = OverlayState::new();
        s.push(Box::new(EscDismiss));
        s.handle_key(&key(PhysicalKey::A));
        assert!(s.is_active(), "non-Esc should not dismiss");
    }

    #[test]
    fn dismiss_pops_only_the_top() {
        let mut s = OverlayState::new();
        s.push(Box::new(EscDismiss));
        s.push(Box::new(EscDismiss));
        assert_eq!(s.depth(), 2);
        s.dismiss();
        assert_eq!(s.depth(), 1, "dismiss pops one, not clear-all");
        assert!(s.is_active());
        s.dismiss();
        assert!(!s.is_active());
    }

    #[test]
    fn handle_key_targets_only_the_top_overlay() {
        // A stay-forever overlay on top must shield the Esc-dismiss
        // overlay beneath it: keys go to the top only.
        struct StayForever;
        impl RenderOverlay for StayForever {
            fn render(&self, _area: Rect, _buf: &mut Buffer) {}
            fn handle_key(&mut self, _key: &KeyEvent) -> OverlayCommand {
                OverlayCommand::Stay
            }
        }
        let mut s = OverlayState::new();
        s.push(Box::new(EscDismiss));
        s.push(Box::new(StayForever));
        s.handle_key(&key(PhysicalKey::Escape));
        assert_eq!(
            s.depth(),
            2,
            "Esc reached the StayForever top, not the EscDismiss beneath"
        );
        // Pop the top; now Esc reaches the EscDismiss overlay.
        s.dismiss();
        s.handle_key(&key(PhysicalKey::Escape));
        assert!(!s.is_active());
    }

    // ---------- phux-foz.7: live row refresh routing ----------

    fn live_fleet_list() -> SelectList {
        let items = vec![SelectItem::new(
            "stale-row",
            phux_config::keybind::ResolvedAction {
                action: "focus-pane".to_owned(),
                args: std::collections::BTreeMap::new(),
            },
        )];
        SelectList::new("agent fleet", items, &crate::render::Theme::default())
            .with_live_key("agent-fleet")
    }

    fn fresh_rows() -> Vec<SelectItem> {
        vec![SelectItem::new(
            "fresh-row",
            phux_config::keybind::ResolvedAction {
                action: "focus-pane".to_owned(),
                args: std::collections::BTreeMap::new(),
            },
        )]
    }

    #[test]
    fn refresh_items_reaches_a_live_overlay() {
        let mut s = OverlayState::new();
        s.push(Box::new(live_fleet_list()));
        assert!(s.refresh_items("agent-fleet", &fresh_rows()));
    }

    #[test]
    fn refresh_items_reaches_a_live_overlay_under_the_top() {
        // A modal stacked on top of the fleet (e.g. help) must not shield
        // it from data refreshes — the walk descends the stack.
        let mut s = OverlayState::new();
        s.push(Box::new(live_fleet_list()));
        s.push(Box::new(EscDismiss));
        assert!(s.refresh_items("agent-fleet", &fresh_rows()));
    }

    #[test]
    fn refresh_items_false_without_a_live_overlay() {
        let mut s = OverlayState::new();
        s.push(Box::new(EscDismiss));
        assert!(!s.refresh_items("agent-fleet", &fresh_rows()));
        // Empty stack: also false.
        let mut s = OverlayState::new();
        assert!(!s.refresh_items("agent-fleet", &fresh_rows()));
    }

    #[test]
    fn paint_composes_stack_bottom_up() {
        // Two overlays writing to distinct cells: both appear. The top
        // overlay overwrites the bottom on any shared cell because it
        // paints last.
        struct WriteAt(u16, &'static str);
        impl RenderOverlay for WriteAt {
            fn render(&self, area: Rect, buf: &mut Buffer) {
                buf.set_string(
                    area.x,
                    area.y + self.0,
                    self.1,
                    ratatui::style::Style::default(),
                );
            }
            fn handle_key(&mut self, _key: &KeyEvent) -> OverlayCommand {
                OverlayCommand::Stay
            }
        }
        let mut s = OverlayState::new();
        s.push(Box::new(WriteAt(0, "bottom")));
        s.push(Box::new(WriteAt(1, "topp")));
        let mut out = Vec::new();
        s.paint(&mut out, (20, 5)).expect("paint");
        let txt = String::from_utf8_lossy(&out);
        assert!(
            txt.contains("bottom"),
            "bottom overlay should be painted:\n{txt}"
        );
        assert!(
            txt.contains("topp"),
            "top overlay should be painted:\n{txt}"
        );
    }

    #[test]
    fn paint_inactive_is_noop() {
        let s = OverlayState::new();
        let mut buf = Vec::new();
        s.paint(&mut buf, (80, 24)).expect("paint");
        assert!(buf.is_empty());
    }

    /// A bounded overlay that paints a sentinel OUTSIDE its bounds (at the
    /// viewport origin) and content INSIDE — so a clipped emit can be proven
    /// to drop the outside sentinel.
    struct Bounded {
        rect: Rect,
    }
    impl RenderOverlay for Bounded {
        fn render(&self, _area: Rect, buf: &mut Buffer) {
            buf.set_string(0, 0, "OUTSIDE", ratatui::style::Style::default());
            buf.set_string(
                self.rect.x,
                self.rect.y,
                "INSIDE",
                ratatui::style::Style::default(),
            );
        }
        fn handle_key(&mut self, _key: &KeyEvent) -> OverlayCommand {
            OverlayCommand::Stay
        }
        fn bounds(&self, _area: Rect) -> Option<Rect> {
            Some(self.rect)
        }
    }

    #[test]
    fn active_bounds_none_for_full_screen_overlay() {
        // EscDismiss uses the default `bounds` (None) ⇒ full-screen path.
        let mut s = OverlayState::new();
        s.push(Box::new(EscDismiss));
        assert_eq!(s.active_bounds(Rect::new(0, 0, 80, 24)), None);
    }

    #[test]
    fn active_bounds_returns_overlay_rect() {
        let mut s = OverlayState::new();
        let rect = Rect::new(5, 3, 10, 4);
        s.push(Box::new(Bounded { rect }));
        assert_eq!(s.active_bounds(Rect::new(0, 0, 40, 12)), Some(rect));
    }

    #[test]
    fn active_bounds_unions_the_stack() {
        let mut s = OverlayState::new();
        s.push(Box::new(Bounded {
            rect: Rect::new(2, 2, 4, 4),
        }));
        s.push(Box::new(Bounded {
            rect: Rect::new(10, 8, 4, 4),
        }));
        // Union bounding box spans x∈[2,14), y∈[2,12).
        assert_eq!(
            s.active_bounds(Rect::new(0, 0, 40, 20)),
            Some(Rect::new(2, 2, 12, 10))
        );
    }

    #[test]
    fn active_bounds_none_if_any_overlay_full_screen() {
        let mut s = OverlayState::new();
        s.push(Box::new(Bounded {
            rect: Rect::new(2, 2, 4, 4),
        }));
        s.push(Box::new(EscDismiss)); // default bounds = None
        assert_eq!(s.active_bounds(Rect::new(0, 0, 40, 20)), None);
    }

    #[test]
    fn paint_clipped_emits_only_within_the_clip() {
        let mut s = OverlayState::new();
        let rect = Rect::new(5, 3, 10, 4);
        s.push(Box::new(Bounded { rect }));
        let mut out = Vec::new();
        // Color::Reset disables the drop shadow ⇒ only the box rows emit.
        s.paint_clipped(
            &mut out,
            (40, 12),
            Rect::new(0, 0, 40, 12),
            rect,
            Color::Reset,
        )
        .expect("paint");
        let txt = String::from_utf8_lossy(&out);
        // Content inside the clip is emitted...
        assert!(txt.contains("INSIDE"), "modal content must paint: {txt:?}");
        // ...but the sentinel at (0,0) — outside the clip — is NOT, so the
        // panes there are left untouched (the floating-modal invariant).
        assert!(
            !txt.contains("OUTSIDE"),
            "cells outside the clip must never be emitted: {txt:?}"
        );
        // The first row CUP targets the clip origin (row 4, col 6, 1-based).
        assert!(
            txt.contains("\x1b[4;6H"),
            "clip-origin CUP missing: {txt:?}"
        );
        // No CUP lands above the clip (row 1) or below it — with the shadow
        // disabled the box bottom (row 7, 0-based) emits no row-8 CUP.
        assert!(
            !txt.contains("\x1b[1;") && !txt.contains("\x1b[8;"),
            "no CUP may target a row outside the clip: {txt:?}"
        );
    }

    #[test]
    fn paint_clipped_draws_a_drop_shadow_below_and_right() {
        let mut s = OverlayState::new();
        let rect = Rect::new(5, 3, 10, 4);
        s.push(Box::new(Bounded { rect }));
        let mut out = Vec::new();
        s.paint_clipped(
            &mut out,
            (40, 12),
            Rect::new(0, 0, 40, 12),
            rect,
            Color::Rgb(20, 20, 30),
        )
        .expect("paint");
        let txt = String::from_utf8_lossy(&out);
        // A shadow row emits just below the box: box bottom row is 6 (0-based)
        // so the shadow row is 7 ⇒ 1-based CUP row 8, started one cell in
        // (x = 6 ⇒ col 7) to skip the bottom-left corner.
        assert!(txt.contains("\x1b[8;7H"), "shadow row CUP missing: {txt:?}");
        // The shadow paints as a truecolor background.
        assert!(
            txt.contains("48;2;20;20;30"),
            "shadow bg SGR missing: {txt:?}"
        );
        // The modal content is still painted over the panes.
        assert!(txt.contains("INSIDE"), "modal content missing: {txt:?}");
        // Still nothing leaks to the viewport origin.
        assert!(!txt.contains("OUTSIDE"), "outside-clip leak: {txt:?}");
    }

    #[test]
    fn paint_active_emits_some_bytes() {
        struct Filled;
        impl RenderOverlay for Filled {
            fn render(&self, area: Rect, buf: &mut Buffer) {
                buf.set_string(area.x, area.y, "hello", ratatui::style::Style::default());
            }
            fn handle_key(&mut self, _key: &KeyEvent) -> OverlayCommand {
                OverlayCommand::Stay
            }
        }
        let mut s = OverlayState::new();
        s.push(Box::new(Filled));
        let mut out = Vec::new();
        s.paint(&mut out, (20, 5)).expect("paint");
        let txt = String::from_utf8_lossy(&out);
        assert!(txt.contains("hello"));
        // Cursor hide at the top.
        assert!(out.starts_with(b"\x1b[?25l"));
    }

    // ---------- phux-fsb: pinned overlays do not outlive a resize ----------

    /// A context menu pinned somewhere in an 80x24 viewport, holding a
    /// destructive row.
    fn pinned_menu() -> ContextMenu {
        let action = |name: &str| phux_config::keybind::ResolvedAction {
            action: name.to_owned(),
            args: std::collections::BTreeMap::new(),
        };
        ContextMenu::new(
            "pane",
            vec![
                MenuRow::item("Close pane", action("kill-pane")),
                MenuRow::item("Zoom", action("toggle-zoom")),
            ],
            (70, 18),
            crate::layout::Rect {
                x: 0,
                y: 0,
                w: 80,
                h: 24,
            },
            &crate::render::Theme::default(),
        )
    }

    /// The reported failure (phux-fsb), stated as the invariant it broke:
    /// after a resize clips a pinned menu away, it must not still be
    /// sitting on the stack turning Enter into `kill-pane`.
    #[test]
    fn a_resize_drops_a_pinned_menu_before_it_can_commit_invisibly() {
        let mut s = OverlayState::new();
        s.push(Box::new(pinned_menu()));
        assert!(s.is_active());

        assert!(
            s.dismiss_stale_on_resize(),
            "the sweep must report a change"
        );
        assert!(!s.is_active(), "the menu is gone, not merely unpainted");

        // The dangerous key is now inert: nothing left to commit it.
        assert_eq!(s.handle_key(&key(PhysicalKey::Enter)), OverlayOutcome::None);
        // And the driver stops paying for hover reporting.
        assert!(!s.wants_pointer_hover());
    }

    /// Reflowing overlays are untouched — they lay out from the render
    /// `area` every paint, so a resize is already handled for them. A sweep
    /// that dropped them would close the palette on every window resize.
    #[test]
    fn a_resize_keeps_overlays_that_reflow() {
        let mut s = OverlayState::new();
        s.push(Box::new(SelectList::new(
            "command palette",
            vec![SelectItem::new(
                "Close pane",
                phux_config::keybind::ResolvedAction {
                    action: "kill-pane".to_owned(),
                    args: std::collections::BTreeMap::new(),
                },
            )],
            &crate::render::Theme::default(),
        )));
        assert!(!s.dismiss_stale_on_resize(), "nothing stale ⇒ no change");
        assert!(s.is_active(), "the palette survives a resize");
    }

    /// The sweep walks the whole stack, so a pinned overlay cannot be
    /// stranded (invisible, input-capturing) beneath a survivor.
    #[test]
    fn a_resize_sweeps_the_whole_stack_not_just_the_top() {
        let mut s = OverlayState::new();
        s.push(Box::new(pinned_menu()));
        s.push(Box::new(EscDismiss));
        assert_eq!(s.depth(), 2);
        assert!(s.dismiss_stale_on_resize());
        assert_eq!(s.depth(), 1, "only the pinned overlay is dropped");
        assert!(s.is_active());
    }

    /// An empty stack is a no-op: SIGWINCH fires constantly during a drag
    /// resize and must not report a change with nothing to drop.
    #[test]
    fn a_resize_with_no_overlays_reports_no_change() {
        let mut s = OverlayState::new();
        assert!(!s.dismiss_stale_on_resize());
        assert!(!s.is_active());
    }

    // ---------- phux-d26y: survivors adopt the new pane size ----------

    /// The fan-out reaches copy-mode through the stack, which is what the
    /// driver relies on — the overlay is behind a `Box<dyn RenderOverlay>`
    /// there, so a method that only existed on the concrete type would
    /// never be called.
    #[test]
    fn a_resize_hands_the_new_pane_size_to_a_surviving_overlay() {
        let mut s = OverlayState::new();
        s.push(Box::new(CopyModeOverlay::new(20, 70, 80, 24)));
        // Selection cursor sits at the old bottom-right region.
        s.on_viewport_resize(60, 18);
        let sel = s.copy_selection().expect("copy-mode has a selection");
        assert!(
            sel.end_row < 18 && sel.end_col < 60,
            "the surviving overlay clamped into the new pane: {sel:?}",
        );
    }

    /// Ordering: the sweep runs first, so an overlay that is about to be
    /// dropped is never handed geometry it will not use. Both calls are
    /// safe in either order, but the driver's sequence is the contract.
    #[test]
    fn the_sweep_runs_before_the_resize_fan_out() {
        let mut s = OverlayState::new();
        s.push(Box::new(pinned_menu()));
        s.push(Box::new(CopyModeOverlay::new(20, 70, 80, 24)));

        assert!(s.dismiss_stale_on_resize(), "the menu is dropped");
        s.on_viewport_resize(60, 18);

        assert_eq!(s.depth(), 1, "only copy-mode survives");
        let sel = s.copy_selection().expect("copy-mode survived");
        assert!(sel.end_row < 18 && sel.end_col < 60);
    }
}
