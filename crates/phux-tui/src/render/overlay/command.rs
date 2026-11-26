//! What an overlay asks for, and what the stack hands the dispatcher.

use super::{CopyMotionRequest, CopyRequest, CopySearchRequest};

/// What an overlay wants the driver to do after [`RenderOverlay::handle_key`](super::RenderOverlay::handle_key).
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
    /// the dispatcher answers through [`OverlayState::apply_copy_search`](super::OverlayState::apply_copy_search).
    Search(CopySearchRequest),
    /// Keep copy-mode active and resolve a cursor move against the focused
    /// pane's text; answered through [`OverlayState::apply_copy_motion`](super::OverlayState::apply_copy_motion).
    Motion(CopyMotionRequest),
    /// Close the overlay and deliver this confirmed paste to the focused pane.
    Paste(phux_protocol::input::paste::PasteEvent),
    /// Close the overlay and set the host clipboard to this text (ADR-0158).
    SetClipboard(phux_client_core::engine::ClipboardText),
}

/// What [`OverlayState::handle_key`](super::OverlayState::handle_key) hands back to the dispatcher.
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
