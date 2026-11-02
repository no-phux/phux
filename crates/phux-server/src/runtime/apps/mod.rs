//! The apps this server hosts, one per resource kind.
//!
//! An app owns everything its kind needs from the host: its facet commands
//! and how a client's stream bootstraps. The runtime reaches a kind only
//! through [`app_for`] and [`app_owning`], so what a kind *is* stays in its
//! app, and a new kind is one [`ResourceApp`] plus one [`APPS`] row. `state`
//! sits below this module: what it needs from any resource is
//! [`crate::resource::ResourceHandle::ops`].

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared across the runtime; pub would violate unreachable_pub"
)]

use std::collections::HashMap;
use std::time::Duration;

use futures_util::future::LocalBoxFuture;
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::ids::{OperationId, ResourceId, ResourceKey, Revision, SatelliteHost, StreamId};
use phux_protocol::wire::frame::{Command, CommandResult, FrameKind, SpawnError, SpawnResource};
use tokio_util::sync::CancellationToken;

use self::terminal::input_lane::{InputCredits, InputLaneHandle};
use super::commands::{AttachResourceFailure, CommandContext};
use super::connections::dispatch::FrameFlow;
use super::connections::hello::NegotiatedConnection;
use super::connections::plumbing::ClientPlumbing;
use super::operation_dedupe::OperationClaim;
use crate::resource::{ResourceHandle, ResourceKind};
use crate::state::upgrade_blob::RebuiltSession;
use crate::state::{ClientId, Outbound, ServerState, SharedState};
use crate::upgrade::blob::{AgentSessionBlob, AgentStreamCut};

pub(crate) mod agent_session;
pub(crate) mod terminal;

/// One client's stream onto one resource, ready to bootstrap.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StreamBootstrap<'a> {
    /// Server state.
    pub(crate) state: &'a SharedState,
    /// The attaching connection.
    pub(crate) client_id: ClientId,
    /// The resource's wire id.
    pub(crate) wire: &'a phux_protocol::ids::ResourceId,
    /// The resource.
    pub(crate) core: phux_core::ids::ResourceId,
    /// Its engine handle.
    pub(crate) handle: &'a ResourceHandle,
    /// Where the stream's frames go.
    pub(crate) content_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    /// The wire stream.
    pub(crate) stream_id: StreamId,
    /// What the client negotiated.
    pub(crate) client_caps: ClientCapabilities,
    /// The negotiated bootstrap profile.
    pub(crate) profile: BootstrapProfile,
    /// The negotiated bootstrap limits.
    pub(crate) limits: BootstrapLimits,
    /// The connection's cancellation.
    pub(crate) connection_token: &'a CancellationToken,
    /// A replay cursor, for kinds that resume from one.
    pub(crate) after_seq: Option<u64>,
}

/// One `SPAWN_RESOURCE` addressed to an app's kind.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResourceSpawn<'a> {
    /// Server state.
    pub(crate) state: &'a SharedState,
    /// The spawning connection.
    pub(crate) client_id: ClientId,
    /// The request to answer.
    pub(crate) request_id: u32,
    /// Kind, parent, and provenance.
    pub(crate) resource: &'a SpawnResource,
    /// Satellite to relay to, `None` for local.
    pub(crate) satellite: Option<&'a SatelliteHost>,
    /// Where the reply and the stream's frames go.
    pub(crate) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    /// The server's cancellation.
    pub(crate) root_token: &'a CancellationToken,
    /// The negotiated bootstrap limits.
    pub(crate) limits: BootstrapLimits,
    /// The connection's cancellation.
    pub(crate) connection_token: &'a CancellationToken,
}

/// The agent-session streams one graceful upgrade carries. From the moment it
/// exists until exec, the host holds it; dropping it releases whatever it
/// still holds, and a successful exec replaces the process instead.
pub(crate) trait StreamUpgrade {
    /// Choose the streams to carry and return the future that cuts and seals
    /// them. The choice happens in this call, not when the future first
    /// runs, so the caller makes it in the same step as its tree identity.
    fn cut<'a>(
        &'a mut self,
        state: &SharedState,
        deadline: Duration,
    ) -> LocalBoxFuture<'a, HashMap<phux_core::ids::ResourceId, AgentStreamCut>>;

    /// A stream that cannot be left behind but has no cut in `cuts`.
    fn uncarried(
        &self,
        state: &ServerState,
        cuts: &HashMap<phux_core::ids::ResourceId, AgentStreamCut>,
    ) -> Option<phux_protocol::ids::ResourceId>;

    /// Reopen every sealed stream within a bounded window; a stream that
    /// misses it is left to drop.
    fn lift(&mut self) -> LocalBoxFuture<'_, ()>;
}

/// What serving one client frame about one resource may touch.
pub(crate) struct ResourceFrame<'a> {
    /// Server state.
    pub(crate) state: &'a SharedState,
    /// The sending connection.
    pub(crate) client_id: ClientId,
    /// The connection's cancellation.
    pub(crate) token: &'a CancellationToken,
    /// The off-thread input lane, when this server runs one.
    pub(crate) input_lane: Option<&'a InputLaneHandle>,
    /// This connection's input credits.
    pub(crate) input_credits: &'a mut InputCredits,
    /// This connection's outbound plumbing.
    pub(crate) plumbing: &'a mut ClientPlumbing,
    /// What the connection negotiated, once it has.
    pub(crate) selection: Option<NegotiatedConnection>,
}

/// What the host asks of the app serving a kind.
pub(crate) trait ResourceApp: Sync {
    /// The kind this app serves.
    fn kind(&self) -> ResourceKind;

    /// Whether a multi-stream connection waits for `STREAM_BIND` before
    /// bootstrapping this kind's content.
    fn defers_stream_bootstrap(&self) -> bool;

    /// Whether `ATTACH_RESOURCE` may carry a replay cursor for this kind.
    fn resumes_from_cursor(&self) -> bool;

    /// May a client take over this kind's input? `Err` names why not.
    fn check_takeover(&self) -> Result<(), crate::resource::WrongResourceKind>;

    /// Drop `client`'s engine-side consumer state now, best effort. Safe under
    /// the state lock.
    fn release_consumer(&self, _handle: &ResourceHandle, _client: ClientId) {}

    /// Drop `client`'s consumer state and wait until the engine has stopped
    /// emitting to it.
    fn fence_consumer<'a>(
        &'a self,
        _handle: &'a ResourceHandle,
        _client: ClientId,
    ) -> LocalBoxFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }

    /// Serve a facet command this kind owns in the catalog.
    fn command<'a>(
        &'a self,
        ctx: &'a CommandContext,
        command: Command,
        claim: Option<&'a OperationClaim>,
    ) -> LocalBoxFuture<'a, CommandResult>;

    /// Serve a client frame about one resource of this kind. A kind that
    /// does not own the frame refuses it with its own typed error.
    fn frame<'a>(
        &'a self,
        request: ResourceFrame<'a>,
        frame: FrameKind,
    ) -> LocalBoxFuture<'a, FrameFlow>;

    /// Bootstrap one client's stream onto a resource of this kind.
    fn bootstrap<'a>(
        &'a self,
        stream: StreamBootstrap<'a>,
    ) -> LocalBoxFuture<'a, Result<(), AttachResourceFailure>>;

    /// Serve a `SPAWN_RESOURCE` of this kind.
    fn spawn<'a>(&'a self, spawn: ResourceSpawn<'a>) -> LocalBoxFuture<'a, ()> {
        Box::pin(super::spawn_reply::refuse_spawn(
            spawn.out_tx,
            spawn.request_id,
            SpawnError::UnsupportedKind,
        ))
    }

    /// The tree came back from an upgrade blob and no client is connected.
    fn resumed(&self, _state: &SharedState) {}

    /// Any resource's engine exited. Runs before the host retires it.
    fn resource_exited<'a>(
        &'a self,
        _state: &'a SharedState,
        _resource: phux_core::ids::ResourceId,
    ) -> LocalBoxFuture<'a, ()> {
        Box::pin(std::future::ready(()))
    }

    /// Serve a relaunch this kind recognizes as its own, leaving the rest to
    /// the organization path.
    fn claim_relaunch(
        &self,
        _ctx: &CommandContext,
        _operation: OperationId,
        _resource: &ResourceKey,
        _expected: Revision,
    ) -> Option<CommandResult> {
        None
    }
}

/// Every app this server hosts.
static APPS: [&dyn ResourceApp; 2] = [&terminal::TerminalApp, &agent_session::AgentSessionApp];

/// The app serving `kind`, or `None` for a kind this build does not serve.
pub(crate) fn app_for(kind: ResourceKind) -> Option<&'static dyn ResourceApp> {
    APPS.iter().copied().find(|app| app.kind() == kind)
}

/// Tell every app the tree came back from an upgrade blob.
pub(crate) fn resumed(state: &SharedState) {
    for app in APPS {
        app.resumed(state);
    }
}

/// Tell every app a resource's engine exited.
pub(crate) async fn resource_exited(state: &SharedState, resource: phux_core::ids::ResourceId) {
    for app in APPS {
        app.resource_exited(state, resource).await;
    }
}

/// The result of the app that claims this relaunch, if one does.
pub(crate) fn claim_relaunch(
    ctx: &CommandContext,
    operation: OperationId,
    resource: &ResourceKey,
    expected: Revision,
) -> Option<CommandResult> {
    APPS.iter()
        .find_map(|app| app.claim_relaunch(ctx, operation, resource, expected))
}

/// Begin carrying agent sessions across an upgrade, or `None` when another
/// upgrade already holds them. Blob-shaped, so one kind fills it: see
/// [`crate::upgrade::blob::StateBlob::agent_sessions`].
pub(crate) fn begin_stream_upgrade(state: &SharedState) -> Option<Box<dyn StreamUpgrade>> {
    agent_session::upgrade::begin(state)
}

/// Rebuild one carried agent session for
/// [`ServerState::rebuild_from_blob`].
pub(crate) fn rebuild_session(
    carried: &AgentSessionBlob,
    parent: phux_core::ids::ResourceId,
    parent_handle: Option<&ResourceHandle>,
    log_bytes: u32,
    token: CancellationToken,
) -> RebuiltSession {
    agent_session::upgrade::rebuild(carried, parent, parent_handle, log_bytes, token)
}

/// The resource a client frame is about; `None` for a frame about none.
pub(crate) const fn frame_resource(frame: &FrameKind) -> Option<&ResourceId> {
    match frame {
        FrameKind::InputKey { terminal_id, .. }
        | FrameKind::InputMouse { terminal_id, .. }
        | FrameKind::InputFocus { terminal_id, .. }
        | FrameKind::InputPaste { terminal_id, .. }
        | FrameKind::InputTerminalReply { terminal_id, .. }
        | FrameKind::ResizeTerminal { terminal_id, .. }
        | FrameKind::FrameAck { terminal_id, .. }
        | FrameKind::HistoryRequest { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}

/// The app serving the resource `frame` is about. An id that resolves to no
/// local resource is a satellite's or a stale one, and only the Terminal app
/// relays or drops those.
pub(crate) fn frame_app(state: &SharedState, frame: &FrameKind) -> &'static dyn ResourceApp {
    frame_resource(frame)
        .and_then(|wire| state.with(|s| Some(s.resource_handle(s.resource_from_wire(wire)?)?.kind)))
        .and_then(app_for)
        .unwrap_or(&terminal::TerminalApp)
}

/// The native bootstrap profiles this server can offer at HELLO.
pub(crate) fn bootstrap_capabilities() -> phux_protocol::caps::BootstrapCapabilities {
    terminal::bootstrap_capabilities()
}

/// The app that owns `command` in the catalog; `None` for a host command.
pub(crate) fn app_owning(command: &Command) -> Option<&'static dyn ResourceApp> {
    app_for(crate::resource::core_kind(
        phux_protocol::kinds::command_kind(command)?,
    )?)
}

#[cfg(test)]
mod seam_tests;
#[cfg(test)]
mod tests;
