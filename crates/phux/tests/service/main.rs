//! Server auto-spawn, service management, and socket recovery integration tests.

#[path = "../common/mod.rs"]
mod common;
#[path = "../common/runner.rs"]
mod runner;

mod autosave_e2e;
mod kill_server_e2e;
mod server_ensure;
mod service_install_guard;
mod service_login_shell_e2e;
mod service_reconcile;
mod stale_socket_recovery;
mod support;
