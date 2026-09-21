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

pub mod agent;
pub mod event;
pub mod grid;
pub mod id;
pub mod outcome;
pub mod status;
pub mod topology;
