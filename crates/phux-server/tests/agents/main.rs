//! Agent detection harness. Its tests set process-wide detector timing
//! overrides, so they stay out of the shared terminal binary.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

mod agent_detect;
