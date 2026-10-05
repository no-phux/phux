//! Lane-selected recording and playback acceptance tests.

#![allow(
    clippy::duplicate_mod,
    reason = "retain each suite's common-module tests and state when consolidating binaries"
)]

#[path = "../common/runner.rs"]
mod runner;

mod play_e2e;
mod rec_e2e;
