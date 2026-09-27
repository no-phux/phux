//! The acknowledged-input replay journal (ADR-0053), which lives in
//! [`phux_client_core::input_replay`], plus the native operation-id minter.

pub use phux_client_core::input_replay::*;
use phux_protocol::ids::InputOperationId;

/// Mint a fresh non-zero 128-bit acknowledged-input operation id.
#[must_use]
pub fn mint_input_operation_id() -> InputOperationId {
    loop {
        if let Some(id) = InputOperationId::new(uuid::Uuid::new_v4().into_bytes()) {
            return id;
        }
    }
}
