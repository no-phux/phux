//! The Terminal's mailboxes: [`TerminalInput`] toward the PTY and
//! [`ControlRequest`] toward the actor.

use phux_protocol::ClientId;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::input::mouse::MouseEvent;
use phux_protocol::input::paste::PasteEvent;
use phux_protocol::wire::frame::TerminalSignal;
use phux_resource::LeaseChange;
use tokio::sync::oneshot;

/// One input event for a pane (`docs/protocol.md#input`).
#[derive(Debug, Clone)]
pub enum TerminalInput {
    /// A keystroke (`INPUT_KEY` on the wire — `docs/protocol.md#input`).
    Key(KeyEvent),
    /// A mouse event (`INPUT_MOUSE` — `docs/protocol.md#input`).
    Mouse(MouseEvent),
    /// A focus gained/lost notification (`INPUT_FOCUS` — `docs/protocol.md#input`).
    Focus(FocusEvent),
    /// A bracketed paste (`INPUT_PASTE` — `docs/protocol.md#input`).
    Paste(PasteEvent),
}

/// A supervisory request to a Terminal's actor (ADR-0033).
///
/// The input lease lives in `ServerState`; the actor emits `TerminalControl`
/// because it owns the lifecycle.
#[derive(Debug)]
pub enum ControlRequest {
    /// The input lease changed; broadcast the new holder and action.
    LeaseChanged(LeaseChange),
    /// Signal the pane's process group, update the lifecycle, and broadcast
    /// `TerminalControl`; `reply` carries delivery or an error.
    Signal {
        /// The signal to deliver.
        signal: TerminalSignal,
        /// The lease holder at the time of the signal, for the broadcast.
        input_holder: Option<ClientId>,
        /// The client requesting the signal.
        by: ClientId,
        /// The signal's `operation_id` (L1 §5.1.1).
        operation_id: Option<phux_protocol::ids::IdempotencyKey>,
        /// Delivery acknowledgement.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// The child exited and the pane is retained (ADR-0124): report
    /// `Exited`, stop the detector, refuse input. No reply.
    Retire,
}
