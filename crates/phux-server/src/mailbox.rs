//! Per-client and per-pane mailbox payloads.
//!
//! [`Outbound`] is what the writer task drains toward a client;
//! [`TerminalInput`] is what a pane's actor drains toward its PTY. Both are
//! pure message shapes over `phux-protocol` atoms — no server state, no actor
//! machinery.
//!
//! They live at the crate root rather than inside `state` on purpose: `state`
//! and `terminal_actor` both need them, and hanging them off either one makes
//! the two subsystems import each other. As a crate-root leaf the dependency
//! is one-way from both sides. `state` re-exports them so existing
//! `crate::state::Outbound` paths keep working.

use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::input::mouse::MouseEvent;
use phux_protocol::input::paste::PasteEvent;

/// Default per-client outbound mailbox depth.
///
/// Bounded on purpose: a stuck client must not let the server accumulate
/// unbounded backpressure. The exact number is small because outbound
/// frames are *coalesced byte chunks* (see `docs/spec/L1.md` §2 and ADR-0013),
/// not individual PTY reads; eight in-flight `TERMINAL_OUTPUT` batches is
/// well above steady state.
pub const DEFAULT_CLIENT_MAILBOX: usize = 8;

/// Per-pane input event recorded against a pane.
///
/// `phux-byc.4` records these into a per-pane log; a future task will turn
/// them into PTY writes. The variant set tracks `docs/spec/input.md` (Input
/// events).
#[derive(Debug, Clone)]
pub enum TerminalInput {
    /// A keystroke (`INPUT_KEY` on the wire — `docs/spec/input.md` §2).
    Key(KeyEvent),
    /// A mouse event (`INPUT_MOUSE` — `docs/spec/input.md` §3).
    Mouse(MouseEvent),
    /// A focus gained/lost notification (`INPUT_FOCUS` — `docs/spec/input.md` §4).
    Focus(FocusEvent),
    /// A bracketed paste (`INPUT_PASTE` — `docs/spec/input.md` §5).
    Paste(PasteEvent),
}

/// A message queued on a client's outbound mailbox.
///
/// The writer task drains a single channel of [`Outbound`] and routes each
/// item via one write path:
///
/// * [`Outbound::Frame`] carries a [`phux_protocol::wire::frame::FrameKind`]
///   and is encoded via `FrameKind::encode` before being written. Per
///   ADR-0008 / ADR-0013 the protocol crate owns the wire types and the
///   server defers to them for any variant — `Hello`, `TerminalOutput`,
///   lifecycle frames, and so on.
///
/// * [`Outbound::TerminalError`] is an ordered terminal sentinel. The writer
///   writes that final `ERROR`, immediately closes the transport, and discards
///   anything producers race into the mailbox after it.
#[derive(Debug)]
pub enum Outbound {
    /// A structured frame; the writer encodes it before writing.
    Frame(phux_protocol::wire::frame::FrameKind),
    /// Final protocol error followed by an immediate transport close.
    TerminalError {
        /// Optional request correlation.
        request_id: Option<u32>,
        /// Protocol error category.
        code: phux_protocol::wire::frame::ErrorCode,
        /// Human-readable failure detail.
        message: String,
    },
}
