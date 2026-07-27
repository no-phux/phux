//! Attach loop — the runtime that makes `phux attach <session>` work.
//!
//! Wires together four collaborators per the phux-9gw.3 design:
//!
//! * [`connection`] — UDS transport plus length-prefixed frame I/O.
//! * [`driver`] — the `tokio::select!` lifecycle, the file that owns the
//!   process's stdout, stdin, and SIGWINCH handles for the duration of the
//!   attach.
//! * [`render`] — VT emission from a local `libghostty_vt::Terminal` /
//!   `RenderState` pair per ADR-0013.
//! * [`input`] — stdin bytes → structured input events for the keybinding
//!   resolver and pane input forwarding.
//!
//! The public entry point is [`run`]. It expects to be called from a tokio
//! current-thread runtime (matching ADR-0003); embedders are responsible for
//! the runtime lifecycle. The function takes over the controlling terminal
//! (raw mode + alt screen) and restores it on every exit path including
//! panic — see [`driver::RawModeGuard`].
//!
//! # Scope
//!
//! This module deliberately does **not** implement:
//!
//! * Predictive local echo — that's phux-9gw.1 layered on top.
//! * `VIEWPORT_RESIZE` — the wire frame doesn't exist yet; tracked under
//!   phux-4hp.
//! * Mouse / bracketed-paste parsing — keyboard input (ASCII, UTF-8,
//!   CSI / SS3 sequences, modifier-bearing chords, Alt-chords) is
//!   handled by [`input::StdinParser`]; mouse reports and bracketed
//!   paste are deferred follow-ups (see the input module docs).

pub mod action_registry;
pub mod actions;
pub mod connection;
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
pub mod input;
pub mod input_dispatch;
mod onboarding;
pub mod paint;
pub mod plugin_actions;
pub mod plugin_panes;
pub mod quic;
// ADR-0060: the `phux --rec` tee. A `Write` wrapper on the one RenderSink the
// driver already threads through the render path, so a recording is exactly
// the bytes the human's glass received.
pub mod record;
pub mod reflow;
mod reload;
pub mod render;
pub mod rendered;
// ADR-0029 §2: the monotone repaint accumulator. Loop-level triggers raise a
// level; the driver drains it once per iteration, so a burst of chrome
// triggers collapses into a single in-place chrome paint instead of N
// full-screen clears.
mod repaint;
pub mod server_frame;
mod stdout_writer;
mod terminal_probe;
pub mod ws;

pub use connection::{CertTrust, Dial, QuicDial, WsDial};
pub use driver::{
    AttachError, run, run_headless_rendered, run_recorded_dial, run_with_predict,
    run_with_predict_dial, run_with_stdout, write_terminal_reset,
};

// Multi-pane composition moved to `phux-client-core` with phux-0fv
// (ADR-0020): the pure layout-tree → pane-rects + divider-cells compute is
// ratatui-free pane-interior code. Re-exported here so the established
// `crate::attach::multi_pane` / `phux_client::attach::multi_pane` paths
// keep resolving for the driver, paint, and server-frame handler.
pub use crate::multi_pane;

/// The output sink the attach driver composites into.
///
/// The driver threads one `&mut` of this through the whole render path
/// (panes, status bar, dividers, overlays, cursor restore). It is a pure
/// byte sink — a blanket impl covers real stdout (the production tty
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
// Status bar lives under `crate::render::chrome::status_bar` post
// phux-5ke.2 (ADR-0020). Re-exported here so external callers (the
// `phux-client::attach::status_bar::*` integration test path included)
// keep working without changing their imports.
pub use crate::render::chrome::status_bar;
pub use crate::render::chrome::status_bar::{Position, StatusBarPainter, make_context};
