//! Attach loop -- the runtime that makes `phux attach <session>` work.
//!
//! Wires together four collaborators per the phux-9gw.3 design:
//!
//! * [`connection`] -- UDS transport plus length-prefixed frame I/O
//!   (owned by `phux_client`, re-exported here).
//! * [`driver`] -- the `tokio::select!` lifecycle, the file that owns the
//!   process's stdout, stdin, and SIGWINCH handles for the duration of the
//!   attach.
//! * [`render`] -- VT emission from a local `libghostty_vt::Terminal` /
//!   `RenderState` pair per ADR-0013.
//! * [`input`] -- stdin bytes -> structured input events for the keybinding
//!   resolver and pane input forwarding (owned by `phux_client`,
//!   re-exported here).
//!
//! The TUI entry point is [`run_with_predict_dial`]. It expects to be called
//! from a tokio current-thread runtime (matching ADR-0003); embedders are
//! responsible for the runtime lifecycle. The function takes over the
//! controlling terminal (raw mode + alt screen) and restores it on every exit
//! path including panic -- see [`driver::RawModeGuard`].
//!
//! # Layering
//!
//! The headless half of the attach vocabulary -- the connection, the
//! transports, the stdin parser, the replay journal, and the
//! [`AttachError`] / [`AttachEnd`] exit vocabulary -- lives in
//! `phux_client::attach` so headless consumers (the agent verbs, `phux-mcp`)
//! never link a terminal. This module re-exports those pieces under the
//! paths the driver has always used, and adds everything that needs a
//! screen: the driver, the painters, the dispatcher, the overlays' input
//! routing, and the `--rec` tee.

pub mod action_registry;
pub mod actions;
// phux-wrnm: what is on each right-click menu (ADR-0058). The overlay that
// renders one lives in `render::overlay::menu`.
mod context_menu;
pub mod copy;
pub mod driver;
mod exec_widgets;
mod fleet;
mod focus;
// phux-foz.11: glass-diff regression + stress tests for the compose
// invariant (no doubled text under rapid window switching / control spam).
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod ghost_stress_tests;
pub mod input_dispatch;
mod onboarding;
pub mod paint;
// phux-4fbs.4: `PaneSlot` and the client-local indices built over it. Shared
// vocabulary the driver and its siblings both read; see the module doc.
mod pane_state;
pub mod plugin_actions;
pub mod plugin_panes;
mod sidebar_zones;
// ADR-0060: the `phux --rec` tee. A `Write` wrapper on the one RenderSink the
// driver already threads through the render path, so a recording is exactly
// the bytes the human's glass received.
pub mod record;
pub mod reflow;
mod reload;
pub mod render;
// `PHUX_RENDER_PROF=1`: per-second paint/flush/compose counters, so a change
// to the paint scheduler is arguable from numbers rather than a screen
// recording. Free (one predicted branch per call site) when unset.
pub(crate) mod render_prof;
pub mod rendered;
// ADR-0029 §2: the monotone repaint accumulator. Loop-level triggers raise a
// level; the driver drains it once per iteration, so a burst of chrome
// triggers collapses into a single in-place chrome paint instead of N
// full-screen clears.
mod repaint;
pub mod server_frame;
mod stdout_writer;
mod terminal_probe;
// phux-l96p.4: the outer terminal's input handle. Split out of the driver
// because "how stdin is read" is a transport concern with its own fallback
// ladder, not part of the loop's state machine.
mod tty_input;

// The headless attach vocabulary, owned by `phux_client` and re-exported so
// the driver and its siblings keep their `crate::attach::...` /
// `super::...` paths (ADR-0100).
pub use phux_client::attach::{
    AttachEnd, AttachError, CertTrust, Dial, InputReplayJournal, QuicDial, WsDial, connection,
    input, input_replay, outcome, quic, ws,
};

pub use driver::{
    run_headless_rendered, run_recorded_dial, run_with_predict_dial, run_with_stdout,
    write_terminal_reset,
};

// Multi-pane composition lives in `phux-client-core` (phux-0fv, ADR-0020):
// the pure layout-tree -> pane-rects + divider-cells compute is ratatui-free
// pane-interior code. Re-exported here so the established
// `crate::attach::multi_pane` path keeps resolving for the driver, paint,
// and the server-frame handler.
pub use crate::multi_pane;

/// The output sink the attach driver composites into.
///
/// The driver threads one `&mut` of this through the whole render path
/// (panes, status bar, dividers, overlays, cursor restore). It is a pure
/// byte sink -- a blanket impl covers real stdout (the production tty
/// path), a `Vec<u8>` capture (tests today, and a future headless agent
/// surface), or any other `Write`. The chrome toolkit's structured types
/// are rasterized to VT bytes before reaching this boundary, so the sink
/// never carries a grid buffer across module lines.
///
/// The composition entry points (`run_with_stdout`, the driver
/// `main_loop`, `handle_server_frame`, `paint_full_frame`,
/// `dispatch_input_events`) are bound on this trait so the seam is named
/// at the boundary; the lower-level byte renderer and chrome painters
/// stay on plain `Write`, since `RenderSink: Write` lets the sink flow
/// down to them unchanged.
pub trait RenderSink: std::io::Write {}
impl<T: std::io::Write + ?Sized> RenderSink for T {}

impl From<render::RenderError> for AttachError {
    fn from(value: render::RenderError) -> Self {
        match value {
            render::RenderError::Io(e) => Self::Io(e),
            render::RenderError::Ghostty(e) => Self::Ghostty(e),
            render::RenderError::KittyReplay(e) => Self::Protocol(e.to_string()),
            // A row the batched read could not decode is a mirror-integrity
            // failure, not an I/O or emulator fault; it reaches the user as a
            // protocol-level complaint rather than a silently short row.
            other @ render::RenderError::UnreadableCell { .. } => Self::Protocol(other.to_string()),
        }
    }
}

// Status bar lives under `crate::render::chrome::status_bar` post
// phux-5ke.2 (ADR-0020). Re-exported here so external callers (the
// `phux_tui::attach::status_bar::*` integration test path included) keep
// working without changing their imports.
pub use crate::render::chrome::status_bar;
pub use crate::render::chrome::status_bar::{Position, StatusBarPainter, make_context};
