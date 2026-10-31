//! Control-plane command handlers and pane spawning.

#![expect(
    clippy::redundant_pub_crate,
    reason = "private command modules re-export crate-scoped handlers through this facade; pub would violate unreachable_pub"
)]

use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::RolePolicy;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ControlAction, DetachReason, ErrorCode,
    FrameKind, InputMode, ResourceLifecycle, StateScope,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use super::input_lane::InputLaneHandle;
use crate::agent_asked::{AskedPayload, AskedSource};
use crate::resource::ResourceHandle;
use crate::runtime::pump;
use crate::state::RoleEffects;
use crate::state::{
    ClientId, Outbound, RelayRoute, ResolvedOwned, ServerInterceptedKey, SharedState,
};
use crate::terminal_actor::ControlRequest;

mod agent_reports;
pub(crate) mod attach_terminal;
mod dispatch;
mod kill;
mod lifecycle;
mod routing;
mod satellite;
pub(crate) mod satellite_lease;
mod state_query;

#[cfg(test)]
use crate::runtime::apps::terminal::commands::input_lease::spawn_input_lease_expiry;
pub use crate::runtime::apps::terminal::commands::seed::*;
use crate::runtime::apps::terminal::commands::signal::{actor_reply, local_terminal};
pub(crate) use crate::runtime::apps::terminal::commands::*;
pub(crate) use agent_reports::*;
pub(crate) use attach_terminal::*;
use attach_terminal::{
    handle_attach_terminal, invalid_agent_cursor, refuse_invalid_role_policy,
    subscribe_deferred_attach,
};
use dispatch::dispatch_local;
pub(crate) use kill::*;
use kill::{handle_kill_resource_if, handle_kill_terminal, kill_attribution};
pub(crate) use lifecycle::*;
use lifecycle::{handle_shutdown, handle_upgrade};
use routing::send_result;
pub(crate) use routing::*;
pub(in crate::runtime) use satellite::*;
use satellite::{handle_satellite_command, relay_stream_establishing};
#[cfg(test)]
use satellite::{reply_satellite_command, resolve_hub_detach_terminal};
use satellite_lease::{
    SatelliteLeaseTarget, relay_satellite_acquire_input, relay_satellite_attach,
    relay_satellite_release_input, relay_satellite_route_input,
};
#[cfg(test)]
use signal::{commit_signal, record_signal_failure};
pub(crate) use state_query::*;
#[cfg(test)]
use state_query::{merge_satellite_resources, satellite_host_inventory};

#[cfg(test)]
mod lease_expiry_task_tests;

#[cfg(test)]
mod hub_detach_fence_tests;

#[cfg(test)]
mod relay_satellite_attach_role_tests;

#[cfg(test)]
mod host_inventory_tests;

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, reason = "tests")]
mod kill_merge_tests;
