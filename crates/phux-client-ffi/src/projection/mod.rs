//! The one derivation of product vocabulary from `phux-client-runtime`
//! (ADR-0133, ADR-0135).
//!
//! Which runtime event is a terminal signal, what a lifecycle answer
//! carries, how an agent record folds into a badge, what a grid frame reads
//! as: decided here once, in binding-neutral Rust (no `#[repr(C)]`, no
//! `uniffi` derive). Encoders only marshal the results. `crate::c` reads
//! [`event`], [`grid`] and [`agent_records`]; its workspace and status views
//! read runtime values directly because its model differs. A new reading
//! either encoder needs belongs here.

pub mod agent;
pub mod agent_records;
pub mod event;
pub mod grid;
pub mod id;
pub mod outcome;
pub mod status;
pub mod topology;
