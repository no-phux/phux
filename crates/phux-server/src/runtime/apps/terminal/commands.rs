//! Terminal-facet commands: screen reads, resize, signals, input, and the
//! input lease. Each refuses other kinds with `WRONG_RESOURCE_KIND`.

#![expect(
    clippy::redundant_pub_crate,
    reason = "private command modules re-export crate-scoped handlers through this facade; pub would violate unreachable_pub"
)]
#![allow(
    unused_imports,
    reason = "the split command modules share this namespace"
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

use crate::resource::{ResourceHandle, WrongResourceKind};
use crate::runtime::apps::terminal::attach::AttachPrepared;
use crate::runtime::apps::terminal::input_lane::InputLaneHandle;
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
use crate::runtime::commands::attach_terminal::native_checkpoint_profile;
use crate::runtime::commands::satellite_lease::relay_satellite_frame;
#[allow(
    clippy::wildcard_imports,
    reason = "shared with the host's command namespace"
)]
use crate::runtime::commands::*;
use crate::runtime::connections::PaneEvents;
use crate::runtime::pump;
use crate::runtime::{spawn_agent_state_drain, spawn_resource_exit_watcher};
use crate::state::RoleEffects;
use crate::state::{
    ClientId, Outbound, RelayRoute, Resolved, ResolvedOwned, ServerInterceptedKey, SharedState,
};
use phux_resource::agent_asked::{AskedPayload, AskedSource};
use phux_terminal::TerminalInput;
use phux_terminal::actor::{
    ConsumerAckRequest, ControlRequest, ResizeRequest, ScreenReply, ScreenRequest, TerminalActor,
    TerminalHandle,
};

mod app;
pub(crate) mod attach_resource;
mod attach_resource_pump;
pub(crate) mod input_lease;
pub(crate) mod input_route;
pub(crate) mod resize;
pub(crate) mod screen;
pub(crate) mod seed;
pub(crate) mod signal;
pub(crate) mod spawn;
pub(crate) mod terminal_input;

pub(crate) use input_lease::*;
pub(crate) use input_route::*;
pub(crate) use resize::*;
pub(crate) use screen::*;
pub(crate) use signal::*;
pub(crate) use spawn::*;
pub(crate) use terminal_input::*;

pub(crate) use app::TerminalApp;
pub(crate) use attach_resource::*;
use attach_resource::{
    AttachResourceGeneration, AttachResourcePumpChannels, AttachResourceSession,
};
use seed::{seed_session, spawn_cwd_of, stamp_spawn_cwd};
use spawn::{exit_policy_for, spawn_actor};
