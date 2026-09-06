//! Server auto-spawn, service management, and socket recovery integration tests.

#![allow(
    clippy::duplicate_mod,
    reason = "retain each suite's common-module tests and state when consolidating binaries"
)]

mod kill_server_e2e;
mod service_install_guard;
mod service_login_shell_e2e;
mod service_reconcile;
mod stale_socket_recovery;
