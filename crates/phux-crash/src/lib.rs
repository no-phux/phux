//! Fatal-signal terminal restore.
//!
//! DERIVED from xAI's `xai-crash-handler` (Apache-2.0) — see this crate's
//! NOTICE and the Cargo.toml header. This file is MODIFIED FROM UPSTREAM:
//! trimmed to the terminal-restore entry points; upstream's crash-blob
//! capture (`install`, `check_previous_crash`, the blob format, and
//! symbolication) is removed.
//!
//! phux's own teardown paths (`RawModeGuard::drop`, the panic hook, and the
//! SIGINT/SIGTERM/SIGHUP arms) already restore the terminal. This crate covers
//! the case none of them can: a *fatal* signal, which does not unwind, so
//! `Drop` never runs and the panic hook is never called. That matters here
//! because `phux-client` is `#![forbid(unsafe_code)]` — its entire crash
//! surface is the native `libghostty-vt` FFI boundary, and a fault in there
//! would otherwise leave the user in raw mode inside the alt screen.
//!
//! On Unix, SIGBUS/SIGSEGV/SIGABRT are hooked via `sigaction(2)` on an
//! alternate signal stack. The handler restores the terminal and re-raises
//! with default disposition, so exit status and core dumps are unchanged.
//! SIGABRT matters because release builds ship with `panic = "abort"`.
//! Every entry point is a no-op on non-Unix targets.

mod handler;
pub mod terminal;

/// Install a SIGSEGV/SIGBUS/SIGABRT handler that restores the terminal, then
/// re-raises with default disposition (preserving core dumps).
///
/// Snapshots the current termios, so call it *before* entering raw mode.
/// Allocates an alternate signal stack (once per process) so the handler
/// still runs after a stack overflow. Starts in termios-only mode; call
/// [`enable_terminal_escape_restore`] once the alt screen is up.
///
/// No-op on non-Unix targets.
pub fn install_terminal_restore_only() {
    handler::install_terminal_restore_only();
}

/// Upgrade the SIGSEGV/SIGBUS/SIGABRT handlers to also write the DEC
/// private-mode resets in [`terminal::RESTORE_SEQ`]. Call when TUI modes are
/// enabled.
pub fn enable_terminal_escape_restore() {
    handler::enable_terminal_escape_restore();
}

/// Downgrade the SIGSEGV/SIGBUS/SIGABRT handlers to termios-only restoration.
/// Call when TUI modes are disabled.
pub fn disable_terminal_escape_restore() {
    handler::disable_terminal_escape_restore();
}
