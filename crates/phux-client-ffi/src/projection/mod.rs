#![allow(
    unreachable_pub,
    reason = "crate-private module; `pub` items are shared with `crate::uniffi`"
)]

//! The derivation of product vocabulary from `phux-client-runtime`
//! (ADR-0133, ADR-0135): which runtime event is a terminal signal, what a
//! lifecycle answer carries, how an agent record folds into a badge, what a
//! grid frame reads as. Binding-neutral Rust; `crate::uniffi` marshals it.

pub mod agent;
pub mod agent_records;
pub mod approvals;
pub mod grid;
pub mod id;
pub mod organization;
pub mod status;
pub mod topology;
