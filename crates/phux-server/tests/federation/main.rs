//! Hub, relay, connector, satellite, and TLS identity integration tests.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

mod hub_relay_federation;
mod hub_runtime;
mod relay_e2e;
mod relay_inner_tls;
mod tls_server_name;
