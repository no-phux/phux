//! Overlay layer: modals, the action finder, and pickers.
//!
//! [`OverlayState`] is a stack: the top captures input and pauses pane
//! flushes (ADR-0020); dismiss triggers a full repaint.

use std::io::{self, Write};

use phux_protocol::input::key::KeyEvent;
use phux_protocol::input::mouse::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use crate::render::{ChromeBreakpoints, Theme};
#[cfg(test)]
use emit::emit_row_span;
use emit::{emit_buffer, emit_buffer_clipped};

pub mod confirm;
pub mod copy_mode;
mod emit;
pub mod line_edit;
pub mod menu;
pub mod pane_picks;
pub mod path_picker;
pub mod prompt;
pub mod select_list;
pub mod selection;
// The settings page (ADR-0101).
pub mod settings;
pub mod toast;
pub mod which_key;
pub mod widgets;

pub use confirm::{ConfirmOverlay, SharedTarget};
pub use copy_mode::{
    CopyModeOverlay, CopyMotionRequest, CopySearchRequest, CopySearchResult, CopySearchView,
};
pub use menu::{ContextMenu, MenuRow};
pub use pane_picks::{PaneNumbers, SidebarCursor};
pub use path_picker::PathPicker;
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

    /// Adopt the column the [`OverlayCommand::Motion`] this overlay asked
    /// for resolved to.
    fn apply_copy_motion(&mut self, _col: u16) {}

    /// Learn the full-screen row now on the focused pane's viewport row 0
    /// (copy-mode keeps a selection's anchor on its cell across scrolls).
    fn set_copy_viewport_top(&mut self, _screen_top: u32) {}

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

    /// Whether this overlay's geometry survives a viewport resize. The
    /// pointer-pinned context menu does not, and is dropped on SIGWINCH.
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

    /// The request id this placeholder overlay ([`ToastOverlay::listing`]) awaits.
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

    /// The panes to number over the view, in their numbers' order
    /// (`display-panes`); `None` draws no numbers.
    fn pane_numbers(&self) -> Option<&[phux_protocol::ResourceId]> {
        None
    }

    /// Take the sidebar's actionable rows (strip-relative row, what it
    /// commits) and return the row to light (`focus-sidebar`).
    fn sidebar_rows(&mut self, _rows: &dyn Fn() -> SidebarRows) -> Option<u16> {
        None
    }
}

/// The sidebar's actionable rows: strip-relative row and what it commits.
pub type SidebarRows = Vec<(u16, phux_config::keybind::ResolvedAction)>;

/// What an overlay wants the driver to do after [`RenderOverlay::handle_key`].
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayCommand {
    /// Keep the overlay active; the key was consumed.
    Stay,
    /// Close the overlay; the driver repaints the panes.
    Dismiss,
    /// Close the overlay and run this action through `run_action`.
    Commit(phux_config::keybind::ResolvedAction),
    /// Run a drill-down action above this overlay, preserving its query and cursor.
    RunAbove(phux_config::keybind::ResolvedAction),
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
    /// Keep copy-mode active and resolve a cursor move against the focused
    /// pane's text; answered through [`OverlayState::apply_copy_motion`].
    Motion(CopyMotionRequest),
    /// Close the overlay and deliver this confirmed paste to the focused pane.
    Paste(phux_protocol::input::paste::PasteEvent),
    /// Close the overlay and set the host clipboard to this text (ADR-0158).
    SetClipboard(phux_client_core::engine::ClipboardText),
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
    /// Resolve a copy-mode cursor move and hand the column back.
    Motion(CopyMotionRequest),
    /// Deliver a paste the user confirmed.
    Paste(phux_protocol::input::paste::PasteEvent),
    /// Set the host clipboard to a pane's write the user allowed.
    SetClipboard(phux_client_core::engine::ClipboardText),
}

/// Stacked overlay state: the top captures input; rendering walks the stack
/// bottom-up.
#[derive(Default)]
pub struct OverlayState {
    /// Bottom-to-top stack; the last element is the input target.
    stack: Vec<Box<dyn RenderOverlay>>,
    /// The attach's `[chrome]` breakpoints, stamped onto every pushed overlay.
    breakpoints: ChromeBreakpoints,
    /// The last press that entered copy-mode, outliving the overlay so a
    /// repeat press counts as a double- or triple-click.
    last_click: Option<copy_mode::ClickRecord>,
}

impl std::fmt::Debug for OverlayState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverlayState")
            .field("depth", &self.stack.len())
            .field("breakpoints", &self.breakpoints)
            .field("last_click", &self.last_click)
            .finish()
    }
}

impl OverlayState {
    /// Count a copy-mode press on `pane`'s `(col, row)` at `now`: 1 for a
    /// single click, 2 for a double, 3 for a triple.
    pub fn count_click(
        &mut self,
        pane: &phux_protocol::ResourceId,
        col: u16,
        row: u16,
        now: std::time::Instant,
    ) -> u8 {
        copy_mode::ClickRecord::press(&mut self.last_click, pane, col, row, now)
    }

    /// Empty state — no overlay active, shipped breakpoints.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            stack: Vec::new(),
            breakpoints: ChromeBreakpoints::DEFAULT,
            last_click: None,
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

    /// The top overlay's numbered panes; see [`RenderOverlay::pane_numbers`].
    #[must_use]
    pub fn pane_numbers(&self) -> Option<&[phux_protocol::ResourceId]> {
        self.stack.last()?.pane_numbers()
    }

    /// The top overlay's lit sidebar row; see [`RenderOverlay::sidebar_rows`].
    pub fn sidebar_rows(&mut self, rows: &dyn Fn() -> SidebarRows) -> Option<u16> {
        self.stack.last_mut()?.sidebar_rows(rows)
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

    /// Pop the top overlay when it is a passthrough notice (a job's
    /// "running" toast), so the job's result replaces it rather than
    /// stacking on it. The which-key panel is passthrough too but stays: it
    /// belongs to the pending prefix, not to the job.
    pub fn dismiss_passthrough_notice(&mut self) {
        if self.top_is_passthrough() && !self.passthrough_escape_cancels_prefix() {
            self.stack.pop();
        }
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
            OverlayCommand::RunAbove(action) => OverlayOutcome::RunAction(action),
            OverlayCommand::Copy(req) => {
                self.dismiss();
                OverlayOutcome::Copy(req)
            }
            OverlayCommand::ScrollViewport(delta) => OverlayOutcome::ScrollViewport(delta),
            OverlayCommand::ReloadConfig => OverlayOutcome::ReloadConfig,
            OverlayCommand::Search(req) => OverlayOutcome::Search(req),
            OverlayCommand::Motion(req) => OverlayOutcome::Motion(req),
            OverlayCommand::Paste(paste) => {
                self.dismiss();
                OverlayOutcome::Paste(paste)
            }
            OverlayCommand::SetClipboard(text) => {
                self.dismiss();
                OverlayOutcome::SetClipboard(text)
            }
        }
    }

    /// Hand a resolved copy-mode cursor column to the top overlay.
    pub fn apply_copy_motion(&mut self, col: u16) {
        if let Some(top) = self.stack.last_mut() {
            top.apply_copy_motion(col);
        }
    }

    /// Tell the top overlay which full-screen row its pane's viewport starts on.
    pub fn set_copy_viewport_top(&mut self, screen_top: u32) {
        if let Some(top) = self.stack.last_mut() {
            top.set_copy_viewport_top(screen_top);
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
