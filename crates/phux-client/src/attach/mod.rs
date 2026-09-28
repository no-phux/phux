//! The headless half of attaching.
//!
//! Reaching a phux server and speaking the wire, with nothing that needs a
//! screen. The interactive attach loop is the
//! `phux-tui` crate, which re-exports this module (ADR-0100); nothing here may
//! depend on it, so `phux-mcp` and the agent verbs never link a terminal.

pub mod connection;
pub mod input;
pub mod input_replay;
pub mod outcome;
pub mod quic;
pub mod ws;

pub use connection::{CertTrust, Dial, QuicDial, WsDial};
pub use input_replay::{InputReplayJournal, mint_input_operation_id};
pub use outcome::{AttachEnd, AttachError};
