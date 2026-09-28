//! Command dispatch, wire ordering, policy, listeners, and adversarial protocol tests.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

mod byc_6_5_keystroke_merge_order;
mod command_dispatch;
mod command_isolation;
mod frame_too_large;
mod open_listener;
mod policy_deny;
mod web_caps_image_gate;
