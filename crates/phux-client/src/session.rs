//! Session-identity writes: `phux rename` and `phux new`'s
//! create-without-attach (ADR-0022 §5).
//!
//! Both are organization commands: `SESSION_RENAME` naming the session's key
//! and the revision read from its record, and `SESSION_CREATE` with the seed
//! pane as its launch. Each carries a fresh `OperationId` (a keyed create's
//! idempotency key is its operation id), and the `COMMITTED` or `REFUSED`
//! reply is the outcome.

mod create;
mod empty;
mod rename;

pub use create::{
    CreateOutcome, CreateSessionError, CreateSessionRequest, create_session, launch_failure_reason,
};
pub use empty::{
    CreateEmptyOutcome, check_session_directory, create_empty_session,
    create_empty_session_with_attrs,
};
pub use rename::{RenameError, rename_checked};

#[cfg(test)]
mod tests;
