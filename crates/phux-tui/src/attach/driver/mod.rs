//! The attach loop driver: connect, HELLO + ATTACH, then `tokio::select!` over
//! the server, stdin, signals, and timers until detach.
//!
//! It owns the connection, the outer terminal's raw mode and alt screen
//! (restored on every exit path, ADR-0003), and a replica `Terminal` plus
//! [`super::render::TerminalRenderer`] per pane (ADR-0013).

#![allow(
    clippy::result_large_err,
    reason = "AttachError carries an io::Error which is naturally large; the variants are mutually exclusive and we never carry the result in a hot loop."
)]

mod chrome;
mod config_ui;
mod entry;
mod headless;
mod loop_exit;
mod loop_state;
mod main_loop;
pub mod native;
mod orphans;
mod overlay_paint;
mod session_io;
mod session_switch;
mod subscriptions;
mod terminal;
#[cfg(test)]
mod tests;
mod viewport;

pub use entry::{connect_for_attach, run_connection, run_dial, run_with_stdout};
pub use headless::{CompositedCapture, record_composited, run_headless_rendered};
pub use terminal::{adopt_alt_screen_handoff, leave_inherited_alt_screen, write_terminal_reset};
