//! The headless half of attaching -- everything a client needs to reach a
//! phux server and speak the wire, and nothing that needs a screen.
//!
//! * [`connection`] -- UDS transport plus length-prefixed frame I/O, the
//!   HELLO negotiation, and the remote dial vocabulary ([`Dial`],
//!   [`QuicDial`], [`WsDial`], [`CertTrust`]).
//! * [`quic`], [`ws`] -- the remote transports over `phux-dial`.
//! * [`input`] -- stdin bytes -> structured input events. The parser is
//!   terminal-free (it consumes bytes and yields libghostty atoms), so the
//!   agent verbs that synthesize keystrokes share it with the TUI.
//! * [`input_replay`] -- the ADR-0053 acknowledged-input replay journal the
//!   remote reconnect lanes carry across attaches.
//! * [`outcome`] -- the attach exit vocabulary ([`AttachError`],
//!   [`AttachEnd`]) every verb reports through.
//!
//! The interactive attach loop -- raw mode, the `tokio::select!` driver, the
//! libghostty replicas, the ratatui chrome, the keybinding dispatcher -- is
//! the `phux-tui` crate, which depends on this module and re-exports it
//! under `phux_tui::attach` (ADR-0100). Nothing here may depend on that
//! crate: the split exists so `phux-mcp` and the agent verbs link a client
//! without linking a terminal.

pub mod connection;
pub mod input;
// ADR-0053: the acknowledged-input replay journal for the remote reconnect
// lanes -- the CLI analogue of phux-mobile's PendingInput queue. Created per
// attach invocation by the CLI's reconnect loop (remote dials only) and
// threaded through the TUI driver like the `--rec` recorder.
pub mod input_replay;
// phux-4fbs.4: the attach exit vocabulary (`AttachError` / `AttachEnd`).
// A leaf module so the many callers that need only the error type never
// form a back-edge into anything heavier.
pub mod outcome;
pub mod quic;
pub mod ws;

pub use connection::{CertTrust, Dial, QuicDial, WsDial};
pub use input_replay::InputReplayJournal;
pub use outcome::{AttachEnd, AttachError};
