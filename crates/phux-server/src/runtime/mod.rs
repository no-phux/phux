//! Server runtime: accept loops, per-client tasks, and terminal actors.
//!
//! One current-thread tokio executor (ADR-0003) drives a
//! [`tokio::task::LocalSet`] (ADR-0014). The runtime binds the UDS
//! (`docs/spec/proto.md` §4) plus any configured remote listeners, and
//! unlinks the socket it bound on shutdown.
#![allow(
    clippy::future_not_send,
    reason = "single-threaded tokio runtime per ADR-0003; Send/Sync not required"
)]

use crate::state::Outbound;
use phux_protocol::wire::frame::{ErrorCode, FrameKind};
use std::time::Duration;
use tracing::trace;

#[cfg(test)]
mod actor_conformance;
pub mod attach;
mod checkout;
pub mod connections;
/// Compatibility name for the server-side connection handlers.
pub use connections as client;
mod command_tasks;
pub mod commands;
mod committed_close;
pub mod delegation;
mod directory;
mod download;
mod ephemeral_listener;
pub mod idempotent_create;
pub mod input_lane;
pub mod keyed_ops;
mod native_agent;
pub mod operation_dedupe;
mod organization;
mod path_search;
pub mod perf_control;
mod process_env;
mod program_status;
/// Shared per-generation state both pane output pumps enforce.
mod pump;
pub mod resource_commands;
mod resume;
pub mod revocation;
#[cfg(test)]
mod revocation_conformance;
mod upgrade;
mod upload;
mod voice;
mod whoami;

#[cfg(test)]
mod approval_matrix;
mod approvals;
mod dispatch_guard;
#[cfg(test)]
mod org_scale_tests;
#[cfg(test)]
mod scope_matrix;
#[cfg(test)]
mod test_support;
mod workload_auth;

pub(crate) use attach::*;
pub use commands::*;
pub(crate) use connections::*;
pub use process_env::ServerEnv;

/// Timeout for the "is the socket still live?" liveness probe used when an
/// existing socket file is encountered during bind.
pub(crate) const STALE_PROBE_TIMEOUT: Duration = Duration::from_millis(50);

mod config;
mod listeners;
mod posture;
mod server;
mod services;
mod startup;

pub use config::{
    MAX_SOCKET_PATH_LEN, RuntimeFlags, ServerConfig, ServerError, validate_socket_path_len,
};
use listeners::DISABLE_AUTO_LISTEN_ENV;
#[cfg(test)]
use listeners::remote::build_quic_listener_for;
use listeners::remote::remote_certificate;
pub use listeners::{DEFAULT_QUIC_PORT, DEFAULT_WS_PORT};
pub use phux_config::socket::default_socket_path;
#[cfg(test)]
use phux_protocol::wire::ListenerDisabledReason;
pub use server::ServerRuntime;

/// The wire form of a server-local client id, saturating at `u32::MAX`.
pub(crate) fn wire_client(client_id: crate::state::ClientId) -> phux_protocol::ids::ClientId {
    phux_protocol::ids::ClientId::new(u32::try_from(client_id.0).unwrap_or(u32::MAX))
}

/// Queue an `ERROR` frame on `out_tx`.
pub(crate) async fn send_error(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    code: ErrorCode,
    message: &str,
) {
    if out_tx
        .send(Outbound::Frame(FrameKind::Error {
            request_id: None,
            code,
            message: message.to_owned(),
        }))
        .await
        .is_err()
    {
        trace!(?code, "ERROR send dropped: writer gone");
    }
}

#[cfg(test)]
mod tests;
