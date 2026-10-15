//! Wire round trips, bootstrap contracts, and malformed-input rejection.
//!
//! Proptest resolves its corpus against the `it` harness root, under
//! `tests/proptest-regressions/wire_contract/`; the historical wire seeds
//! live there.

mod bootstrap_wire;
mod input_terminal_reply_wire;
mod journal_shapes;
mod keyed_signal_shapes;
mod org_shapes;
mod wire_layout_recursion;
mod wire_roundtrip;
