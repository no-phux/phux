//! Control-plane command handlers and pane spawning.

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

/// One connection's negotiated dispatch context: everything a command needs
/// to run as that connection's, now or later (a held command, ADR-0128).
pub(super) struct CommandContext {
    pub(super) state: SharedState,
    pub(super) client_id: ClientId,
    pub(super) out_tx: tokio::sync::mpsc::Sender<Outbound>,
    pub(super) client_caps: ClientCapabilities,
    pub(super) profile: BootstrapProfile,
    pub(super) limits: BootstrapLimits,
    pub(super) input_lane: Option<InputLaneHandle>,
    /// This connection's cancellation.
    pub(super) token: CancellationToken,
    pub(super) root_token: CancellationToken,
    /// QUIC multi-stream (proto.md §4.2): subscribe now, bootstrap when the
    /// client binds a stream (`STREAM_BIND`).
    pub(super) defer_subscription: bool,
}

#[cfg(test)]
impl CommandContext {
    /// `client_id`'s context on a fresh Unix-socket connection.
    pub(super) fn for_test(
        state: &SharedState,
        client_id: ClientId,
        out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    ) -> Self {
        Self {
            state: state.clone(),
            client_id,
            out_tx: out_tx.clone(),
            client_caps: ClientCapabilities::default(),
            profile: BootstrapProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
            input_lane: None,
            token: CancellationToken::new(),
            root_token: CancellationToken::new(),
            defer_subscription: false,
        }
    }
}

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

pub mod pane_launch;
pub(crate) use pane_launch::*;
pub mod pane_create;
pub use pane_create::*;
pub mod geometry;
pub(crate) use geometry::*;
pub mod dispatch;
pub(crate) use dispatch::*;
pub mod attach_stream;
pub(crate) use attach_stream::*;
pub mod attach_generation;
use attach_generation::AttachResourceSession;
pub mod server_control;
use server_control::{handle_shutdown, handle_upgrade};
pub mod satellite_commands;
pub(super) use satellite_commands::*;
pub mod satellite_leases;
use satellite_leases::{
    SatelliteLeaseTarget, relay_satellite_acquire_input, relay_satellite_attach,
    relay_satellite_frame, relay_satellite_release_input, relay_satellite_route_input,
};
pub mod close_batch;
pub(crate) use close_batch::*;
pub mod inventory;
pub(crate) use inventory::*;
pub mod inspection;
pub(crate) use inspection::*;
pub mod input_leases;
pub(crate) use input_leases::*;
pub mod agent_reports;
pub(crate) use agent_reports::*;
pub mod input_dispatch;
pub(crate) use input_dispatch::*;
