//! Wire round trips, bootstrap contracts, and malformed-input rejection.
//! Snapshot naming and allocation instrumentation keep their standalone roots.
//!
//! Proptest resolves its corpus beside this harness directory, under
//! `tests/proptest-regressions/`; the historical wire seeds live there.

#[path = "../common/mod.rs"]
mod common;

mod bootstrap_wire;
mod input_terminal_reply_wire;
mod wire_layout_recursion;
mod wire_roundtrip;
