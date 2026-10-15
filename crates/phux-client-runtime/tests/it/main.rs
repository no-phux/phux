//! phux-client-runtime integration tests in one binary: the control plane
//! and organization lanes driven sans-IO, and the runtime against a real
//! server over a Unix socket. `perf_counters` reads process-wide counters, so
//! it keeps its own binary.

#[path = "../support/embedded.rs"]
mod embedded;

mod control_plane;
mod e2e_uds;
mod organization;
mod organization_bulk;
