//! Control-plane command handlers and pane spawning.

#![expect(
    clippy::redundant_pub_crate,
    reason = "private command modules re-export crate-scoped handlers through this facade; pub would violate unreachable_pub"
)]

use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::ids::ResourceId;
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::RolePolicy;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ControlAction, DetachReason, ErrorCode,
    FrameKind, InputMode, ResourceLifecycle, StateScope, TerminalSignal, ViewportInfo,
};
use std::collections::HashSet;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use super::client::PaneEvents;
use super::input_lane::InputLaneHandle;
use super::{AttachPrepared, spawn_agent_state_drain, spawn_terminal_exit_watcher};
use crate::agent_asked::{AskedPayload, AskedSource};
use crate::resource::{ResourceHandle, WrongResourceKind};
use crate::runtime::pump;
use crate::state::RoleEffects;
use crate::state::{
    ClientId, Outbound, RelayRoute, Resolved, ResolvedOwned, ServerInterceptedKey, SharedState,
    TerminalInput,
};
use crate::terminal_actor::{
    ConsumerAckRequest, ControlRequest, ResizeRequest, ScreenReply, ScreenRequest, TerminalActor,
    TerminalHandle,
};

mod agent_reports;
mod attach_resource;
mod attach_resource_pump;
mod attach_terminal;
mod dispatch;
mod input_lease;
mod input_route;
mod kill;
mod lifecycle;
mod resize;
mod routing;
mod satellite;
mod satellite_lease;
mod screen;
mod seed;
mod signal;
mod spawn;
mod state_query;
mod terminal_input;

pub(crate) use agent_reports::*;
pub(crate) use attach_resource::*;
use attach_resource::{
    AttachResourceGeneration, AttachResourcePumpChannels, AttachResourceSession,
};
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
use attach_terminal::native_checkpoint_profile;
pub(crate) use attach_terminal::*;
use attach_terminal::{
    handle_attach_terminal, invalid_agent_cursor, refuse_invalid_role_policy,
    subscribe_deferred_attach,
};
use dispatch::dispatch_local;
#[cfg(test)]
use input_lease::spawn_input_lease_expiry;
pub(crate) use input_lease::*;
pub(crate) use input_route::*;
use input_route::{LeaseGrant, terminal_handle};
pub(crate) use kill::*;
use kill::{handle_kill_resource_if, handle_kill_terminal, kill_attribution};
pub(crate) use lifecycle::*;
use lifecycle::{handle_shutdown, handle_upgrade};
pub(crate) use resize::*;
use resize::{send_wrong_kind_error, try_resize};
use routing::send_result;
pub(crate) use routing::*;
pub(in crate::runtime) use satellite::*;
use satellite::{handle_satellite_command, relay_stream_establishing};
#[cfg(test)]
use satellite::{reply_satellite_command, resolve_hub_detach_terminal};
use satellite_lease::{
    SatelliteLeaseTarget, relay_satellite_acquire_input, relay_satellite_attach,
    relay_satellite_frame, relay_satellite_release_input, relay_satellite_route_input,
};
pub(crate) use screen::*;
pub use seed::*;
use seed::{seed_session, spawn_cwd_of, stamp_spawn_cwd};
pub(crate) use signal::*;
use signal::{actor_reply, local_terminal};
#[cfg(test)]
use signal::{commit_signal, record_signal_failure};
pub(crate) use spawn::*;
use spawn::{exit_policy_for, spawn_actor};
pub(crate) use state_query::*;
#[cfg(test)]
use state_query::{merge_satellite_resources, satellite_host_inventory};
pub(crate) use terminal_input::*;

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
