//! Mailbox payloads: [`Outbound`] toward a client, [`TerminalInput`] toward
//! a pane's PTY. A crate-root leaf so `state` and the actor need not import
//! each other.

use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::input::mouse::MouseEvent;
use phux_protocol::input::paste::PasteEvent;

/// Default per-client outbound mailbox depth (frames are coalesced chunks,
/// so eight is ample).
pub const DEFAULT_CLIENT_MAILBOX: usize = 8;

/// One input event for a pane (`docs/spec/input.md`).
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

/// A message on a client's outbound mailbox.
///
/// [`Outbound::Frame`] is encoded and written; [`Outbound::TerminalError`]
/// is a final `ERROR`, then `DETACHED(PROTOCOL_ERROR)` and a close, after
/// which the writer discards anything queued.
#[derive(Debug)]
pub enum Outbound {
    /// A structured frame; the writer encodes it before writing.
    Frame(phux_protocol::wire::frame::FrameKind),
    /// Final protocol error and `DETACHED(PROTOCOL_ERROR)`, then transport close.
    TerminalError {
        /// Optional request correlation.
        request_id: Option<u32>,
        /// Protocol error category.
        code: phux_protocol::wire::frame::ErrorCode,
        /// Human-readable failure detail.
        message: String,
    },
}
