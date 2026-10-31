//! [`TerminalInput`]: the mailbox payload toward a pane's PTY.

use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::input::mouse::MouseEvent;
use phux_protocol::input::paste::PasteEvent;

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
