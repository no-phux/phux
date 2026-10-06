//! Attach loop: the runtime behind `phux attach <session>`.
//!
//! The entry point is [`run_with_predict_dial`], called from a tokio
//! current-thread runtime (ADR-0003); it takes over the controlling terminal
//! and restores it on every exit path including panic. The headless half of
//! the vocabulary (connection, transports, stdin parser, replay journal,
//! [`AttachError`] / [`AttachEnd`]) lives in `phux_client::attach` and is
//! re-exported here; this module adds everything that needs a screen.

pub mod action_registry;
pub mod actions;
mod agent_rows;
mod attach_role;
mod chrome_ctx;
// What is on each right-click menu (ADR-0058). The overlay that
// renders one lives in `render::overlay::menu`.
mod context_menu;
pub mod copy;
mod directory_picker;
pub mod driver;
mod exec_widgets;
mod fleet;
mod focus;
pub mod hosts;
mod path_picker;
// phux-foz.11: glass-diff regression + stress tests for the compose
// invariant (no doubled text under rapid window switching / control spam).
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod ghost_stress_tests;
pub mod input_dispatch;
mod onboarding;
pub mod paint;
// `PaneSlot` and the client-local indices built over it. Shared
// vocabulary the driver and its siblings both read; see the module doc.
mod pane_state;
pub mod plugin_actions;
pub mod plugin_panes;
pub mod plugin_sidebar;
mod review;
mod sidebar_zones;
// ADR-0060: the `phux --rec` tee.
pub mod record;
pub mod reflow;
pub mod render;
// `PHUX_RENDER_PROF=1`: per-second paint/flush/compose counters.
pub(crate) mod render_prof;
pub mod rendered;
// ADR-0029: the monotone repaint accumulator, drained once per iteration.
mod repaint;
pub mod server_frame;
// phux-jx39.3: the session state server frames fold into.
mod session_mirror;
mod stdout_writer;
mod terminal_probe;
mod tty_input;
pub mod update_notice;

// The headless attach vocabulary, owned by `phux_client` (ADR-0100).
pub use phux_client::attach::{
    AttachEnd, AttachError, CertTrust, Dial, InputReplayJournal, QuicDial, WsDial, connection,
    input, input_replay, outcome, quic, ws,
};

pub use driver::{
    connect_for_attach, run_headless_rendered, run_recorded_connection, run_recorded_dial,
    run_with_predict_connection, run_with_predict_dial, run_with_stdout, write_terminal_reset,
};

// ADR-0127: the CLI declares `--viewer` / `--take` once, before it dials.
pub use attach_role::set_attach_role;

pub use crate::multi_pane;

/// The output sink the attach driver composites into.
///
/// A pure byte sink (stdout, a `Vec<u8>` capture, any `Write`) threaded
/// through the whole render path; chrome is rasterized to VT bytes first.
pub trait RenderSink: std::io::Write {}
impl<T: std::io::Write + ?Sized> RenderSink for T {}

impl From<render::RenderError> for AttachError {
    fn from(value: render::RenderError) -> Self {
        match value {
            render::RenderError::Io(e) => Self::Io(e),
            render::RenderError::Ghostty(e) => Self::Ghostty(e),
            render::RenderError::KittyReplay(e) => Self::Protocol(e.to_string()),
            // A row the batched read could not decode is a mirror-integrity
            // failure, reported as a protocol error.
            other @ render::RenderError::UnreadableCell { .. } => Self::Protocol(other.to_string()),
        }
    }
}

pub use crate::render::chrome::status_bar;
