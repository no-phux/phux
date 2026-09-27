//! Overlay layer: modals, the action finder, and pickers.
//!
//! An active overlay captures input and pauses pane stdout flushes (ADR-0020)
//! while the pane mirrors keep ingesting; dismiss triggers a full repaint.
//! [`OverlayState`] is a stack: the top captures input, rendering walks it
//! bottom-up.

use std::io::{self, Write};

use phux_protocol::input::key::KeyEvent;
use phux_protocol::input::mouse::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::render::{ChromeBreakpoints, Theme};

pub mod copy_mode;
pub mod line_edit;
pub mod menu;
pub mod path_picker;
pub mod pending;
pub mod prompt;
pub mod select_list;
pub mod selection;
// The settings page (ADR-0101).
pub mod settings;
pub mod toast;
pub mod which_key;
pub mod widgets;

pub use copy_mode::{CopyModeOverlay, CopySearchRequest, CopySearchResult, CopySearchView};
pub use menu::{ContextMenu, MenuRow};
pub use path_picker::PathPicker;
pub use pending::PendingOverlay;
pub use prompt::PromptOverlay;
pub use select_list::{SelectItem, SelectList};
pub use settings::SettingsOverlay;
// The shared copy-mode selection contract (ADR-0045): the selection UX and
// the renderer import these from one owner so they cannot disagree.
pub use selection::{
    CopyMarks, CopyRequest, ScreenSelectionPoint, SearchMatch, SelectionGrab, SelectionMode,
    SelectionRect,
};
pub use toast::ToastOverlay;
pub use which_key::WhichKeyOverlay;

/// Test double: records every key handed to it and never dismisses.
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
/// It paints into a ratatui [`Buffer`] over the viewport and takes protocol input atoms
/// (ADR-0006/0008), deciding when it is done via [`OverlayCommand`].
pub trait RenderOverlay {
    /// Paint into `buf` covering `area` (usually the full viewport).
    fn render(&self, area: Rect, buf: &mut Buffer);

    /// React to a key: [`OverlayCommand::Dismiss`] closes, `Stay` consumes.
    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand;

    /// Insert clipboard text literally (control characters already removed);
    /// overlays without a text field ignore it.
    fn handle_paste(&mut self, _text: &str) {}

    /// React to a mouse event (most modals ignore pointer input).
    fn handle_mouse(&mut self, _mouse: &MouseEvent) -> OverlayCommand {
        OverlayCommand::Stay
    }

    /// The painted region inside `area`, or `None` for a full-viewport
    /// overlay. A bounded overlay floats: the driver repaints the live panes
    /// and emits only this region on top.
    fn bounds(&self, _area: Rect) -> Option<Rect> {
        None
    }

    /// The active copy-mode selection (pane-local cells), or `None`. Copy
    /// mode is a highlight over the live pane, not a modal: the driver
    /// repaints the pane with these cells inverted.
    fn copy_selection(&self) -> Option<SelectionRect> {
        None
    }

    /// Copy-mode search hits to mark on the pane, or `None`.
    fn copy_search_view(&self) -> Option<CopySearchView<'_>> {
        None
    }

    /// Copy-mode search text for the status strip, or `None`.
    fn copy_search_status(&self) -> Option<String> {
        None
    }

    /// Adopt the result of the [`OverlayCommand::Search`] this overlay asked
    /// for; overlays that never search ignore it.
    fn apply_copy_search(&mut self, _result: CopySearchResult) {}

    /// A display-only overlay (which-key) that never captures input: the
    /// dispatcher dismisses it and processes the event as if it were absent.
    fn is_input_passthrough(&self) -> bool {
        false
    }

    /// Whether Escape cancels the pending prefix for this passthrough overlay
    /// (which-key only).
    fn passthrough_escape_cancels_prefix(&self) -> bool {
        false
    }

    /// Adopt the attach-wide `[chrome]` breakpoints (stamped by
    /// [`OverlayState::push`]); overlays not using `centered_panel` ignore it.
    fn set_breakpoints(&mut self, _bp: ChromeBreakpoints) {}

    /// Adopt a reloaded [`Theme`] (overlays copy their colors at
    /// construction; the settings page shows theme edits live).
    fn set_theme(&mut self, _theme: &Theme) {}

    /// Hover-tracks the pointer with no button held (the context menu): the
    /// driver raises any-motion reporting (`?1003h`) only while one is up.
    fn wants_pointer_hover(&self) -> bool {
        false
    }

    /// Whether this overlay's geometry survives a viewport resize. Centered
    /// overlays reflow from `area` every paint; the context menu pins its box
    /// to the pointer, so it returns `false` and is dropped on SIGWINCH
    /// ([`OverlayState::dismiss_stale_on_resize`]) rather than capture keys
    /// off screen.
    fn survives_resize(&self) -> bool {
        true
    }

    /// Re-derive cached geometry from the focused pane's new size after a
    /// resize (copy-mode clamps its cursor and edges); default no-op.
    fn on_viewport_resize(&mut self, _pane_cols: u16, _pane_rows: u16) {}

    /// Offer a rebuilt row set tagged `key`; a live overlay (the fleet
    /// dashboard) replaces its rows and returns `true`. Push, not poll.
    fn refresh_items(&mut self, _key: &str, _items: &[SelectItem]) -> bool {
        false
    }

    /// The request id this placeholder overlay ([`PendingOverlay`]) awaits.
    fn pending_request(&self) -> Option<u32> {
        None
    }

    /// Active host-path discovery query, if this is a path picker.
    fn path_search(&self) -> Option<(&str, &str)> {
        None
    }

    /// Refresh one active path picker with a correlated host reply.
    fn update_paths(&mut self, _result: &phux_protocol::wire::frame::PathQueryResult) -> bool {
        false
    }
}

/// What an overlay wants the driver to do after [`RenderOverlay::handle_key`].
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayCommand {
    /// Keep the overlay active; the key was consumed.
    Stay,
    /// Close the overlay; the driver repaints the panes.
    Dismiss,
    /// Close the overlay and run this action through `run_action`.
    Commit(phux_config::keybind::ResolvedAction),
    /// Close the overlay and copy the selection to the host clipboard via
    /// OSC 52, resolved client-side (ADR-0030).
    Copy(CopyRequest),
    /// Keep the overlay active and scroll the focused pane's client-local
    /// viewport by `delta` rows (negative means up into scrollback).
    ScrollViewport(isize),
    /// Keep the overlay and reload the config in place (the settings page
    /// wrote the file, ADR-0101).
    ReloadConfig,
    /// Keep copy-mode active and search the focused pane's loaded history;
    /// the dispatcher answers through [`OverlayState::apply_copy_search`].
    Search(CopySearchRequest),
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
    /// The overlay wrote the config file; run the in-place reload while the
    /// overlay remains active.
    ReloadConfig,
    /// Search the focused pane for copy-mode and hand the result back.
    Search(CopySearchRequest),
}

/// Stacked overlay state: the top captures input; rendering walks the stack
/// bottom-up.
#[derive(Default)]
pub struct OverlayState {
    /// Bottom-to-top stack; the last element is the input target.
    stack: Vec<Box<dyn RenderOverlay>>,
    /// The attach's `[chrome]` breakpoints, stamped onto every pushed overlay.
    breakpoints: ChromeBreakpoints,
}

impl std::fmt::Debug for OverlayState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayState")
            .field("depth", &self.stack.len())
            .field("breakpoints", &self.breakpoints)
            .finish()
    }
}

impl OverlayState {
    /// Empty state — no overlay active, shipped breakpoints.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            stack: Vec::new(),
            breakpoints: ChromeBreakpoints::DEFAULT,
        }
    }

    /// Adopt the attach's `[chrome]` breakpoints, re-stamping stacked
    /// overlays so a reload reaches an open modal.
    pub fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
        for overlay in &mut self.stack {
            overlay.set_breakpoints(bp);
        }
    }

    /// Re-stamp every stacked overlay with a reloaded [`Theme`]; see
    /// [`RenderOverlay::set_theme`].
    pub fn set_theme(&mut self, theme: &Theme) {
        for overlay in &mut self.stack {
            overlay.set_theme(theme);
        }
    }

    /// `true` when at least one overlay is active.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        !self.stack.is_empty()
    }

    /// The top overlay's copy-mode selection, when copy-mode is on top.
    #[must_use]
    pub fn copy_selection(&self) -> Option<SelectionRect> {
        self.stack.last().and_then(|o| o.copy_selection())
    }

    /// Number of overlays currently stacked (0 when inactive).
    #[cfg(test)]
    #[must_use]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// `true` when the top overlay is input-passthrough (which-key).
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

    /// `true` when any stacked overlay hover-tracks the pointer.
    #[must_use]
    pub fn wants_pointer_hover(&self) -> bool {
        self.stack.iter().any(|o| o.wants_pointer_hover())
    }

    /// Drop every overlay a resize invalidated
    /// ([`RenderOverlay::survives_resize`]) anywhere in the stack, like native
    /// menus close on resize. `true` when the stack changed.
    pub fn dismiss_stale_on_resize(&mut self) -> bool {
        let before = self.stack.len();
        self.stack.retain(|overlay| overlay.survives_resize());
        self.stack.len() != before
    }

    /// Hand every surviving overlay the focused pane's new size; runs after
    /// [`Self::dismiss_stale_on_resize`].
    pub fn on_viewport_resize(&mut self, pane_cols: u16, pane_rows: u16) {
        for overlay in &mut self.stack {
            overlay.on_viewport_resize(pane_cols, pane_rows);
        }
    }

    /// Push `overlay` on top, stamped with the attach's breakpoints.
    pub fn push(&mut self, mut overlay: Box<dyn RenderOverlay>) {
        overlay.set_breakpoints(self.breakpoints);
        self.stack.push(overlay);
    }

    /// The request id the top overlay is a placeholder for, if any.
    #[must_use]
    pub fn top_pending_request(&self) -> Option<u32> {
        self.stack.last()?.pending_request()
    }

    /// Never update a hidden picker below another modal.
    #[must_use]
    pub fn path_search(&self) -> Option<(&str, &str)> {
        self.stack.last()?.path_search()
    }

    /// Refresh only the active path picker; a covered or dismissed one stays inert.
    pub fn update_paths(&mut self, result: &phux_protocol::wire::frame::PathQueryResult) -> bool {
        self.stack
            .last_mut()
            .is_some_and(|top| top.update_paths(result))
    }

    /// `true` while some stacked placeholder awaits `request_id`.
    #[must_use]
    pub fn awaits(&self, request_id: u32) -> bool {
        self.stack
            .iter()
            .any(|overlay| overlay.pending_request() == Some(request_id))
    }

    /// Replace the top overlay only when it is the placeholder for
    /// `request_id`: a reply for a dismissed or covered placeholder opens
    /// nothing (it would steal the user's next keystroke).
    pub fn replace_pending(&mut self, request_id: u32, overlay: Box<dyn RenderOverlay>) -> bool {
        if self.top_pending_request() != Some(request_id) {
            return false;
        }
        self.stack.pop();
        self.push(overlay);
        true
    }

    /// Pop the top overlay (no-op when empty).
    pub fn dismiss(&mut self) {
        self.stack.pop();
    }

    /// Dispatch a key to the top overlay (see `settle`).
    pub fn handle_key(&mut self, key: &KeyEvent) -> OverlayOutcome {
        let Some(top) = self.stack.last_mut() else {
            return OverlayOutcome::None;
        };
        let command = top.handle_key(key);
        self.settle(command)
    }

    /// Turn the top overlay's command into the dispatcher's outcome, popping
    /// it on `Dismiss`, `Commit`, and `Copy` (tmux-style copy-and-exit).
    fn settle(&mut self, command: OverlayCommand) -> OverlayOutcome {
        match command {
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
                self.dismiss();
                OverlayOutcome::Copy(req)
            }
            OverlayCommand::ScrollViewport(delta) => OverlayOutcome::ScrollViewport(delta),
            OverlayCommand::ReloadConfig => OverlayOutcome::ReloadConfig,
            OverlayCommand::Search(req) => OverlayOutcome::Search(req),
        }
    }

    /// Hand a copy-mode search result to the top overlay.
    pub fn apply_copy_search(&mut self, result: CopySearchResult) {
        if let Some(top) = self.stack.last_mut() {
            top.apply_copy_search(result);
        }
    }

    /// The top overlay's copy-mode search hits, if it is searching.
    #[must_use]
    pub fn copy_search_view(&self) -> Option<CopySearchView<'_>> {
        self.stack.last().and_then(|o| o.copy_search_view())
    }

    /// The top overlay's copy-mode search status text.
    #[must_use]
    pub fn copy_search_status(&self) -> Option<String> {
        self.stack.last().and_then(|o| o.copy_search_status())
    }

    /// Insert a paste into the top overlay, control characters removed so a
    /// pasted Enter or Escape never becomes a command.
    pub fn handle_paste(&mut self, text: &str) {
        if let Some(top) = self.stack.last_mut() {
            let text: String = text.chars().filter(|ch| !ch.is_control()).collect();
            top.handle_paste(&text);
        }
    }

    /// Hand a rebuilt row set to the stack top-down; `true` when a live
    /// overlay accepted it.
    pub fn refresh_items(&mut self, key: &str, items: &[SelectItem]) -> bool {
        self.stack
            .iter_mut()
            .rev()
            .any(|overlay| overlay.refresh_items(key, items))
    }

    /// Dispatch a mouse event to the top overlay (see `settle`).
    pub fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayOutcome {
        let Some(top) = self.stack.last_mut() else {
            return OverlayOutcome::None;
        };
        let command = top.handle_mouse(mouse);
        self.settle(command)
    }

    /// The union of the stacked overlays' bounds, centered inside `content`
    /// (the pane content rect, so a modal never covers the sidebar), or
    /// `None` when any overlay is full-screen (the driver then clears and
    /// paints the whole viewport).
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

    /// Paint the stack bottom-up into a full-viewport buffer and emit it.
    /// Not a diff: callers clear the screen first.
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

    /// Paint the stack against `content` but emit only the cells inside
    /// `clip` plus a one-cell `shadow` below and right (`Color::Reset`
    /// disables it): the floating-modal path over already-painted panes.
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

/// A pointer position as a viewport cell: negative or NaN is 0, beyond
/// `u16::MAX` saturates (a malformed report never breaks routing).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=u16::MAX first"
)]
fn pointer_cell(p: f64) -> u16 {
    p.max(0.0).min(f64::from(u16::MAX)) as u16
}

/// Emit a buffer as VT bytes (width-aware rows, coalesced style runs).
fn emit_buffer(out: &mut impl Write, buf: &Buffer) -> io::Result<()> {
    let area = buf.area;
    // Hide cursor for the duration of the modal paint.
    out.write_all(b"\x1b[?25l")?;
    for row in 0..area.height {
        emit_row_span(out, buf, area.y + row, area.x, area.right())?;
    }
    // Park the (hidden) cursor; the pane repaint on dismiss re-shows it.
    out.write_all(b"\x1b[1;1H")?;
    out.flush()
}

/// Emit only the cells of `buf` inside `clip`, plus the drop shadow, each
/// row `CUP`-positioned at its own left edge. The shadow's two outer corners are
/// skipped so it reads as an L.
fn emit_buffer_clipped(
    out: &mut impl Write,
    buf: &mut Buffer,
    clip: Rect,
    shadow: Color,
) -> io::Result<()> {
    if clip.width == 0 || clip.height == 0 {
        return Ok(());
    }
    let bands = paint_shadow_bands(buf, clip, shadow);
    out.write_all(b"\x1b[?25l")?;
    emit_clipped_rows(out, buf, clip, bands)?;
    // Park the (hidden) cursor at the modal origin; the next pane repaint on
    // dismiss emits its own DECTCEM.
    write!(out, "\x1b[{};{}H", clip.y + 1, clip.x + 1)?;
    out.flush()
}

/// Which of the drop shadow's two bands the viewport has room for.
#[derive(Clone, Copy)]
struct ShadowBands {
    /// The column just right of the box (`clip.x + clip.width`).
    col: bool,
    /// The row just below the box (`clip.y + clip.height`).
    row: bool,
}

impl ShadowBands {
    /// The shadow bands exist only where there's a pane cell to cast onto.
    const fn for_clip(area: Rect, clip: Rect, shadow: Color) -> Self {
        Self {
            col: !matches!(shadow, Color::Reset) && clip.x + clip.width < area.width,
            row: !matches!(shadow, Color::Reset) && clip.y + clip.height < area.height,
        }
    }
}

/// Paint the drop shadow's bands into `buf` as `shadow`-bg spaces, and report
/// which of them landed.
fn paint_shadow_bands(buf: &mut Buffer, clip: Rect, shadow: Color) -> ShadowBands {
    let vp_w = buf.area.width;
    let vp_h = buf.area.height;
    let bands = ShadowBands::for_clip(buf.area, clip, shadow);
    let rx = clip.x + clip.width; // box right edge (exclusive) = shadow column
    let ry = clip.y + clip.height; // box bottom edge (exclusive) = shadow row
    let style = ratatui::style::Style::default().bg(shadow);
    if bands.col {
        // Right band: beside the box's lower rows + the bottom-right corner.
        for y in (clip.y + 1)..=ry.min(vp_h - 1) {
            fill_shadow_cell(buf, rx, y, style);
        }
    }
    if bands.row {
        // Bottom band: beneath the box, starting one cell in (skip the corner).
        for x in (clip.x + 1)..=rx.min(vp_w - 1) {
            fill_shadow_cell(buf, x, ry, style);
        }
    }
    bands
}

/// Overwrite one in-bounds cell with a blank carrying the shadow style.
fn fill_shadow_cell(buf: &mut Buffer, x: u16, y: u16, style: ratatui::style::Style) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_symbol(" ");
        cell.set_style(style);
    }
}

/// Emit the box's rows plus the bottom shadow band, each with a leading CUP
/// to its own left edge so only the box (and shadow band) cells are written.
fn emit_clipped_rows(
    out: &mut impl Write,
    buf: &Buffer,
    clip: Rect,
    bands: ShadowBands,
) -> io::Result<()> {
    let bx = clip.x;
    let by = clip.y;
    let rx = clip.x + clip.width;
    let ry = clip.y + clip.height;
    // Box rows. The top row omits the right shadow cell (no shadow above the
    // box); lower rows extend one cell right to include the shadow column.
    for row in by..ry {
        let end = if bands.col && row > by { rx + 1 } else { rx };
        emit_row_span(out, buf, row, bx, end)?;
    }
    // Bottom shadow row, skipping the bottom-left corner (start at bx + 1).
    if bands.row {
        let end = if bands.col { rx + 1 } else { rx };
        emit_row_span(out, buf, ry, bx + 1, end)?;
    }
    Ok(())
}

/// Emit cells `[start_col, end_col)` of `row`, `CUP`-positioned at the start, with
/// per-cell SGR deltas.
fn emit_row_span(
    out: &mut impl Write,
    buf: &Buffer,
    row: u16,
    start_col: u16,
    end_col: u16,
) -> io::Result<()> {
    write!(out, "\x1b[{};{}H", row + 1, start_col + 1)?;
    out.write_all(b"\x1b[0m")?;
    let mut prev_styled = None;
    let mut col = start_col;
    while col < end_col {
        let cell = &buf[(col, row)];
        crate::render::sgr::emit_cell_sgr(out, cell, &mut prev_styled)?;
        let sym = cell.symbol();
        let width = u16::try_from(crate::render::display_width(sym))
            .unwrap_or(1)
            .max(1);
        if sym.is_empty() || width > end_col - col {
            out.write_all(b" ")?;
            col += 1;
        } else {
            out.write_all(sym.as_bytes())?;
            col += width;
        }
    }
    if prev_styled.is_some() {
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

    fn action(name: &str) -> phux_config::keybind::ResolvedAction {
        phux_config::keybind::ResolvedAction {
            action: name.to_owned(),
            args: std::collections::BTreeMap::new(),
        }
    }

    /// A test overlay: optionally Esc-dismissable, optionally bounded, and
    /// painting `text` at `rect`'s origin plus an `OUTSIDE` sentinel at the
    /// viewport origin when bounded.
    #[derive(Default)]
    struct Probe {
        esc_dismisses: bool,
        rect: Option<Rect>,
        text: &'static str,
    }

    impl Probe {
        fn esc() -> Self {
            Self {
                esc_dismisses: true,
                ..Self::default()
            }
        }

        fn bounded(rect: Rect) -> Self {
            Self {
                rect: Some(rect),
                text: "INSIDE",
                ..Self::default()
            }
        }
    }

    impl RenderOverlay for Probe {
        fn render(&self, area: Rect, buf: &mut Buffer) {
            let style = ratatui::style::Style::default();
            match self.rect {
                Some(rect) => {
                    buf.set_string(0, 0, "OUTSIDE", style);
                    buf.set_string(rect.x, rect.y, self.text, style);
                }
                None => {
                    buf.set_string(area.x, area.y, self.text, style);
                }
            }
        }
        fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
            if self.esc_dismisses && key.key == PhysicalKey::Escape {
                OverlayCommand::Dismiss
            } else {
                OverlayCommand::Stay
            }
        }
        fn bounds(&self, _area: Rect) -> Option<Rect> {
            self.rect
        }
    }

    fn live_fleet_list() -> SelectList {
        SelectList::new(
            "agent fleet",
            vec![SelectItem::new("stale-row", action("focus-pane"))],
            &Theme::default(),
        )
        .with_live_key("agent-fleet")
    }

    #[test]
    fn keys_reach_only_the_top_and_dismiss_pops_one() {
        let mut s = OverlayState::new();
        assert!(!s.is_active());
        s.push(Box::new(Probe::esc()));
        s.handle_key(&key(PhysicalKey::A));
        assert!(s.is_active(), "non-Esc stays");
        // A stay-forever top shields the Esc-dismiss overlay beneath it.
        s.push(Box::new(Probe::default()));
        s.handle_key(&key(PhysicalKey::Escape));
        assert_eq!(s.depth(), 2);
        s.dismiss();
        assert_eq!(s.depth(), 1, "dismiss pops one");
        s.handle_key(&key(PhysicalKey::Escape));
        assert!(!s.is_active());
    }

    /// The refresh walks the whole stack, so a modal on top cannot shield a
    /// live list beneath it; without one it reports no change.
    #[test]
    fn refresh_items_reaches_a_live_overlay_anywhere_in_the_stack() {
        let fresh = vec![SelectItem::new("fresh-row", action("focus-pane"))];
        let mut s = OverlayState::new();
        assert!(!s.refresh_items("agent-fleet", &fresh));
        s.push(Box::new(live_fleet_list()));
        s.push(Box::new(Probe::esc()));
        assert!(s.refresh_items("agent-fleet", &fresh));
        let mut s = OverlayState::new();
        s.push(Box::new(Probe::esc()));
        assert!(!s.refresh_items("agent-fleet", &fresh));
    }

    #[test]
    fn paint_composes_the_stack_bottom_up() {
        let mut s = OverlayState::new();
        let mut buf = Vec::new();
        s.paint(&mut buf, (80, 24)).expect("paint");
        assert!(buf.is_empty(), "inactive paints nothing");
        s.push(Box::new(Probe {
            text: "bottom",
            ..Probe::default()
        }));
        s.push(Box::new(Probe {
            rect: None,
            text: "to",
            ..Probe::default()
        }));
        let mut out = Vec::new();
        s.paint(&mut out, (20, 5)).expect("paint");
        let txt = String::from_utf8_lossy(&out);
        assert!(out.starts_with(b"\x1b[?25l"), "cursor hidden first");
        assert!(txt.contains("tottom"), "top paints over bottom: {txt:?}");
    }

    /// Records the breakpoints it is stamped with, exposed through `bounds`.
    #[derive(Default)]
    struct BreakpointProbe(Option<ChromeBreakpoints>);

    impl RenderOverlay for BreakpointProbe {
        fn render(&self, _area: Rect, _buf: &mut Buffer) {}
        fn handle_key(&mut self, _key: &KeyEvent) -> OverlayCommand {
            OverlayCommand::Stay
        }
        fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
            self.0 = Some(bp);
        }
        fn bounds(&self, _area: Rect) -> Option<Rect> {
            self.0
                .map(|bp| Rect::new(0, 0, bp.compact_cols, bp.compact_rows))
        }
    }

    /// `push` stamps the configured breakpoints on the way in, and a reload
    /// re-stamps overlays already open.
    #[test]
    fn breakpoints_reach_pushed_and_already_stacked_overlays() {
        let viewport = Rect::new(0, 0, 200, 60);
        let mut s = OverlayState::new();
        s.push(Box::new(BreakpointProbe::default()));
        assert_eq!(
            s.active_bounds(viewport),
            Some(Rect::new(0, 0, 64, 18)),
            "shipped"
        );
        s.set_breakpoints(ChromeBreakpoints {
            compact_cols: 100,
            compact_rows: 40,
            min_pane_cols: 30,
        });
        assert_eq!(s.active_bounds(viewport), Some(Rect::new(0, 0, 100, 40)));
        s.dismiss();
        s.push(Box::new(BreakpointProbe::default()));
        assert_eq!(s.active_bounds(viewport), Some(Rect::new(0, 0, 100, 40)));
    }

    #[test]
    fn active_bounds_unions_bounded_overlays_and_yields_to_full_screen() {
        let mut s = OverlayState::new();
        let viewport = Rect::new(0, 0, 40, 20);
        s.push(Box::new(Probe::bounded(Rect::new(2, 2, 4, 4))));
        assert_eq!(s.active_bounds(viewport), Some(Rect::new(2, 2, 4, 4)));
        s.push(Box::new(Probe::bounded(Rect::new(10, 8, 4, 4))));
        assert_eq!(s.active_bounds(viewport), Some(Rect::new(2, 2, 12, 10)));
        s.push(Box::new(Probe::esc()));
        assert_eq!(
            s.active_bounds(viewport),
            None,
            "any full-screen overlay wins"
        );
    }

    /// The floating-modal invariant: only cells inside the clip (and its
    /// shadow) are emitted, so the panes around the box stay untouched.
    #[test]
    fn paint_clipped_emits_only_the_box_and_its_shadow() {
        let rect = Rect::new(5, 3, 10, 4);
        let mut s = OverlayState::new();
        s.push(Box::new(Probe::bounded(rect)));
        let paint = |shadow| {
            let mut out = Vec::new();
            s.paint_clipped(&mut out, (40, 12), Rect::new(0, 0, 40, 12), rect, shadow)
                .expect("paint");
            String::from_utf8_lossy(&out).into_owned()
        };
        let plain = paint(Color::Reset);
        assert!(
            plain.contains("INSIDE") && !plain.contains("OUTSIDE"),
            "{plain:?}"
        );
        assert!(plain.contains("\x1b[4;6H"), "clip-origin CUP: {plain:?}");
        assert!(
            !plain.contains("\x1b[1;") && !plain.contains("\x1b[8;"),
            "{plain:?}"
        );

        // The shadow row sits below the box, one cell in, in the shadow bg.
        let shadowed = paint(Color::Rgb(20, 20, 30));
        assert!(shadowed.contains("\x1b[8;7H"), "{shadowed:?}");
        assert!(shadowed.contains("48;2;20;20;30"), "{shadowed:?}");
        assert!(!shadowed.contains("OUTSIDE"), "{shadowed:?}");
    }

    #[test]
    fn unicode_vt_paint_keeps_borders_on_grid_in_full_and_clipped_paths() {
        use crate::attach::render::{ReplicaWalk, TerminalRenderer};
        use libghostty_vt::Terminal;
        let mut buf = Buffer::empty(Rect::new(0, 0, 24, 4));
        buf.set_string(
            3,
            1,
            "│构建工具 cafe\u{301}│",
            ratatui::style::Style::default(),
        );
        for clipped in [false, true] {
            let mut out = Vec::new();
            if clipped {
                emit_row_span(&mut out, &buf, 1, 3, 20).expect("clipped paint");
            } else {
                emit_buffer(&mut out, &buf).expect("full paint");
            }
            let mut terminal = Terminal::new(24, 4).expect("terminal");
            terminal
                .set_scrollback_max_lines(Some(0))
                .expect("terminal");
            terminal.vt_write(&out);
            let mut renderer = TerminalRenderer::new().expect("renderer");
            for (col, ch) in [
                (3, '│'),
                (4, '构'),
                (6, '建'),
                (8, '工'),
                (10, '具'),
                (17, '│'),
            ] {
                assert_eq!(
                    renderer
                        .read_grapheme_at(ReplicaWalk::for_test(&terminal), 1, col)
                        .expect("cell"),
                    Some(ch),
                    "clipped={clipped}, col={col}"
                );
            }
        }
    }

    /// A context menu pinned in an 80x24 viewport, holding a destructive row.
    fn pinned_menu() -> ContextMenu {
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
            &Theme::default(),
        )
    }

    /// A resize drops a pointer-pinned menu (it could otherwise commit
    /// `kill-pane` invisibly on Enter) anywhere in the stack, keeps reflowing
    /// overlays, and then hands survivors the new pane size.
    #[test]
    fn a_resize_drops_pinned_overlays_and_resizes_survivors() {
        let mut s = OverlayState::new();
        assert!(!s.dismiss_stale_on_resize(), "empty: no change");
        s.push(Box::new(pinned_menu()));
        s.push(Box::new(SelectList::new(
            "command palette",
            vec![SelectItem::new("Close pane", action("kill-pane"))],
            &Theme::default(),
        )));
        assert!(s.dismiss_stale_on_resize());
        assert_eq!(s.depth(), 1, "only the pinned menu is dropped");
        assert!(!s.wants_pointer_hover());
        assert!(!s.dismiss_stale_on_resize(), "nothing stale left");
        s.dismiss();
        assert_eq!(s.handle_key(&key(PhysicalKey::Enter)), OverlayOutcome::None);

        s.push(Box::new(pinned_menu()));
        s.push(Box::new(CopyModeOverlay::new(20, 70, 80, 24)));
        assert!(s.dismiss_stale_on_resize());
        s.on_viewport_resize(60, 18);
        let sel = s.copy_selection().expect("copy-mode survived");
        assert!(sel.end_row < 18 && sel.end_col < 60, "{sel:?}");
    }
}
