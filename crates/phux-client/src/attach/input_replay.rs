//! Compatibility surface for the shared acknowledged-input replay policy.
//!
//! The state machine lives in [`phux_client_core::input_replay`] so native
//! hosts can consume it without this async transport crate. This module keeps
//! the established `phux_client::attach::input_replay` path and owns only the
//! native operation-id generator used by the reference TUI.

pub use phux_client_core::input_replay::*;
use phux_protocol::ids::InputOperationId;

/// Mint a fresh non-zero 128-bit acknowledged-input operation id.
///
/// Hosts may use their platform CSPRNG instead and pass the resulting id to
/// [`InputReplayJournal::submit`] or [`InputReplayJournal::submit_at`].
#[must_use]
pub fn mint_input_operation_id() -> InputOperationId {
    loop {
        if let Some(id) = InputOperationId::new(uuid::Uuid::new_v4().into_bytes()) {
            return id;
        }
    }
}
