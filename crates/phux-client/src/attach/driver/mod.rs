//! The attach loop driver: connect, HELLO + ATTACH, then `tokio::select!`
//! over the server, stdin, SIGWINCH, and the detach chord until the server
//! sends `DETACHED` or the user requests detach.
//!
//! The driver owns:
//!
//! * the [`super::connection::Connection`] (UDS transport),
//! * stdout via a [`RawModeGuard`] that flips the outer terminal into raw
//!   mode + alt screen on construction and restores it on drop (panic-safe
//!   per ADR-0003's "no hung outer terminals" requirement),
//! * a stdin reader,
//! * a SIGWINCH listener (currently a no-op; once `VIEWPORT_RESIZE` lands
//!   in phux-4hp it will start sending resize frames upstream),
//! * a local `libghostty_vt::Terminal` + [`super::render::TerminalRenderer`] for the focused
//!   pane (under ADR-0013 the client is bytes-in / `vt_write` / dirty-row
//!   redraw — see `research/2026-05-25-libghostty-renderstate.md`).

#![allow(
    clippy::result_large_err,
    reason = "AttachError carries an io::Error which is naturally large; the variants are mutually exclusive and we never carry the result in a hot loop."
)]

mod chrome;
mod config_ui;
mod entry;
mod headless;
mod main_loop;
mod overlay_paint;
mod session_io;
mod subscriptions;
mod terminal;
#[cfg(test)]
mod tests;
mod viewport;

pub use entry::{
    run_recorded_dial, run_with_predict_dial, run_with_stdout, run_with_stdout_predict,
};
pub use headless::run_headless_rendered;
pub use terminal::{RawModeGuard, write_terminal_reset};
