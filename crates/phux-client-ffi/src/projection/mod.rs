//! The one derivation of product vocabulary from `phux-client-runtime`.
//!
//! Every decision a binding used to make on its own — which runtime event is
//! a terminal signal, what a lifecycle answer carries, how an agent record
//! folds into a state and an attention level, what a grid frame reads as —
//! is made here, once, in binding-neutral Rust. There is no `#[repr(C)]` and
//! no `uniffi` derive in this module: the C encoder in `crate::c` and the
//! `UniFFI` encoder in `crate::uniffi` marshal these values into their own
//! vocabularies and nothing else.
//!
//! That is the rule ADR-0133 states and ADR-0135 enforces: one runtime, one
//! interpretation of it, however many languages consume the result.
//!
//! Today [`event`]'s signal and lifecycle families and [`grid`]'s frame
//! reading are read by both `crate::c` and `crate::uniffi` — that is the
//! drift ADR-0135 exists to close. [`topology`], [`status`], [`outcome`],
//! [`agent`] and [`id`] are derived once here but currently consumed only by
//! `crate::uniffi`; `crate::c`'s workspace and status views still read
//! `phux-client-runtime` values directly (`c::workspace::session_summaries`,
//! `c::client`'s `RuntimeStatus` mapping), because its model genuinely
//! differs. If the C encoder
//! ever needs a topology, status or outcome reading, it belongs here, not
//! re-derived in `crate::c` — that re-derivation is exactly what this module
//! exists to make impossible.

pub mod agent;
pub mod event;
pub mod grid;
pub mod id;
pub mod outcome;
pub mod status;
pub mod topology;
