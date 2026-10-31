//! `ATTACH`, `SPAWN_RESOURCE`, and `MOVE_RESOURCE` handling, plus the per-pane
//! output pumps that publish bootstrap generations and live output.

#![expect(
    clippy::redundant_pub_crate,
    reason = "private attach modules re-export crate-scoped handlers through this facade; pub would violate unreachable_pub"
)]

use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use phux_core::ResourceId;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ClientCapabilities,
};
use phux_protocol::ids::{BootstrapId, GroupId, StreamId};
use phux_protocol::wire::frame::{
    AttachTarget, DetachReason, ErrorCode, FrameKind, MAX_AGENT_SESSION_RECORD_BYTES, MoveError,
    MoveResult, SpawnError, SpawnResult,
};
use std::ops::ControlFlow;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use tracing::{debug, trace, warn};

use crate::hub::relay::SatelliteSpawn;
use crate::resource::ResourceHandle;
use crate::runtime::pump::{self, PumpGeneration};
use crate::runtime::{
    SpawnOwnership, prepare_attach, seed_session_with_pty_and_colors, send_error,
    spawn_pane_with_pty_and_colors,
};
use crate::state::{AttachSnapshotPane, ClientId, Outbound, SharedState};
use crate::terminal_actor::{
    ConsumerAttachRequest, ConsumerDetachRequest, PaneOutput, PwdRequest, ResizeRequest,
    ResizeSender, ResyncAudience, ResyncTarget, SetDefaultColorsRequest, SnapshotRequest,
};

mod bootstrap;
mod bootstrap_queue;
mod handle;
mod move_terminal;
mod output_pump;
mod output_pump_resync;
mod pane_capture;
mod pane_capture_run;
mod prepare;
mod publication;
mod pump_loop;
mod spawn;
mod spawn_plan;
mod spawn_publication;
mod target;

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
use bootstrap::publishes_native_checkpoints;
pub(crate) use bootstrap::*;
use bootstrap::{BootstrapStagingBudget, SnapshotGate, initial_bootstrap_id};
#[cfg(test)]
use bootstrap_queue::GAP_RESYNC_QUEUE_WAIT;
pub(crate) use bootstrap_queue::*;
pub(crate) use handle::*;
pub(crate) use move_terminal::*;
pub(crate) use output_pump::*;
use output_pump::{PaneResync, tombstone_reason_for};
use pane_capture::PaneCaptureContext;
use prepare::{
    AttachStaging, CaptureFailure, ConsumerRegistration, apply_client_default_colors,
    apply_session_role, detach_prior_state_sync_consumers, is_same_session_reattach,
    prepare_attach_or_refuse,
};
use publication::AttachPublication;
#[cfg(test)]
use pump_loop::next_wait;
pub(crate) use pump_loop::*;
use pump_loop::{GapCause, fail_aggregate_attach_prepublication, run_output_pump};
pub(crate) use spawn::*;
pub(crate) use spawn_plan::*;
use spawn_plan::{
    PaneSpawnPlan, build_spawn_command, forwarded_resource, log_spawn_request,
    refuse_vanished_pane_handle, resolve_spawn_ownership, spawn_argv_builder,
    spawn_pane_or_refusal, spawn_terminal_output_pump, subscribe_spawning_client,
    usable_initial_size, validate_local_spawn,
};
use spawn_publication::SpawnPublication;
pub(crate) use target::*;

#[cfg(test)]
mod tests;
