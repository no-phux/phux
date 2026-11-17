//! `phux mission`: JSON goldens and the schema, `mission start`, and the
//! keeper's projection of agent state into task state.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in an integration test"
)]

#[path = "../common/runner.rs"]
mod runner;

#[path = "../common/mod.rs"]
mod common;

mod golden;
mod json;
mod project;
mod start;
mod support;
