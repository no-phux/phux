//! The Terminal app: one PTY child behind a libghostty terminal.
//!
//! Everything that knows what a terminal *is* lives here: the engine actor
//! ([`actor`]), the canonical grid and its replay ([`grid`]), input encoding
//! ([`input`]), native checkpoints ([`native_state`]), and agent detection
//! over the screen ([`agent_detect`]). The crate depends on the kind-neutral
//! [`phux_resource`] substrate and never on the server that hosts it.

#![allow(
    clippy::redundant_pub_crate,
    reason = "items shared inside the crate; pub would widen the app's API"
)]

pub mod actor;
pub mod agent_detect;
pub mod grid;
pub mod input;
pub mod mailbox;
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub mod native_state;
pub mod perf;

/// The process spawn vocabulary the host builds a pane's launch from.
pub mod pty {
    pub use portable_pty::CommandBuilder;
}

/// The terminal library's error, carried by input encoding.
pub use libghostty_vt::Error as VtError;

pub use actor::{
    SnapshotRequest, TerminalActor, TerminalActorBundle, TerminalActorError, TerminalHandle,
};
pub use mailbox::{ControlRequest, TerminalInput};
pub use phux_resource::mailbox::Outbound;
