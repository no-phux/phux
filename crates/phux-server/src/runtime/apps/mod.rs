//! The apps this server hosts, one per resource kind.
//!
//! An app owns everything its kind needs from the host: its facet commands,
//! how a client's stream bootstraps, and the stream and input frames about
//! its resources. The runtime reaches a kind only
//! through [`app_for`] and [`app_owning`], so what a kind *is* stays in its
//! app. A new kind does not compile until [`app_for`] names its app, and
//! every [`ResourceApp`] method is required, so no kind inherits an answer
//! by omission. `state` sits below this module: what it needs from any
//! resource is [`crate::resource::ResourceHandle::ops`].

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared across the runtime; pub would violate unreachable_pub"
)]

use std::collections::HashMap;
use std::time::Duration;

use futures_util::future::LocalBoxFuture;
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::ids::{
    OperationId, ResourceId as WireResourceId, ResourceKey, Revision, SatelliteHost, StreamId,
};
use phux_protocol::wire::frame::{Command, CommandResult, FrameKind, SpawnResource};
use tokio_util::sync::CancellationToken;

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
pub(crate) mod document;
pub(crate) mod terminal;

use terminal::input_lane::{InputCredits, InputLaneHandle};

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
    /// The negotiated Terminal bootstrap profile; `None` on a connection
    /// that offered none, which a Terminal refuses and other kinds ignore.
    pub(crate) profile: Option<BootstrapProfile>,
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

/// One already-committed launch effect. The app must execute this exact
/// resource activation, never substitute another kind's default launch.
#[derive(Clone, Copy)]
pub(crate) struct ResourceLaunch<'a> {
    pub(crate) state: &'a SharedState,
    pub(crate) resource: phux_core::ids::ResourceId,
    pub(crate) spec: &'a phux_core::LaunchSpec,
    pub(crate) root_token: &'a CancellationToken,
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

/// One client's stream or input frame about a resource, with what the
/// connection it arrived on lends the app to serve it.
pub(crate) struct ResourceFrame<'a> {
    /// Server state.
    pub(crate) state: &'a SharedState,
    /// The sending connection.
    pub(crate) client_id: ClientId,
    /// The connection's cancellation.
    pub(crate) token: &'a CancellationToken,
    /// The connection's input lane, when it has one.
    pub(crate) input_lane: Option<&'a InputLaneHandle>,
    /// The connection's mailboxes: control, and a bound stream per resource.
    pub(crate) plumbing: &'a ClientPlumbing,
    /// What `HELLO` negotiated, once it has.
    pub(crate) negotiated: Option<NegotiatedConnection>,
    /// The connection's pane input credits.
    pub(crate) input_credits: &'a mut InputCredits,
}

/// The host-side check an app needs before it sees a local frame. These
/// distinctions preserve the wire's input, hint, and refusal semantics.
#[derive(Clone, Copy)]
pub(crate) enum FrameAdmission {
    /// No attachment is needed (e.g. exact resize or an in-band refusal).
    Unrestricted,
    /// Only a live subscriber may receive this frame's answer.
    Subscription,
    /// Attached input also respects retained exits, guardians, and leases.
    AttachedInput,
}

/// What the host asks of the app serving a kind.
pub(crate) trait ResourceApp: Sync {
    /// Whether ordinary organization creation/relaunch can execute this kind.
    /// Producer-bound projections instead open through their producer's app.
    fn supports_organization_launch(&self) -> bool;

    /// Execute a committed launch. A failure is reported against the effect's
    /// activation by the host; it cannot undo the accepted operation.
    fn launch(&self, launch: ResourceLaunch<'_>) -> Result<(), String>;

    /// Whether a multi-stream connection waits for `STREAM_BIND` before
    /// bootstrapping this kind's content.
    fn defers_stream_bootstrap(&self) -> bool;

    /// Whether `ATTACH_RESOURCE` may carry a replay cursor for this kind.
    fn resumes_from_cursor(&self) -> bool;

    /// May a client take over this kind's input? `Err` names why not.
    fn check_takeover(&self) -> Result<(), crate::resource::WrongResourceKind>;

    /// Drop `client`'s engine-side consumer state now, best effort. Safe under
    /// the state lock.
    fn release_consumer(&self, handle: &ResourceHandle, client: ClientId);

    /// Drop `client`'s consumer state and wait until the engine has stopped
    /// emitting to it.
    fn fence_consumer<'a>(
        &'a self,
        handle: &'a ResourceHandle,
        client: ClientId,
    ) -> LocalBoxFuture<'a, ()>;

    /// Serve a facet command this kind owns in the catalog.
    fn command<'a>(
        &'a self,
        ctx: &'a CommandContext,
        command: Command,
        claim: Option<&'a OperationClaim>,
    ) -> LocalBoxFuture<'a, CommandResult>;

    /// Bootstrap one client's stream onto a resource of this kind.
    fn bootstrap<'a>(
        &'a self,
        stream: StreamBootstrap<'a>,
    ) -> LocalBoxFuture<'a, Result<(), AttachResourceFailure>>;

    /// Serve a `SPAWN_RESOURCE` of this kind.
    fn spawn<'a>(&'a self, spawn: ResourceSpawn<'a>) -> LocalBoxFuture<'a, ()>;

    /// The tree came back from an upgrade blob and no client is connected.
    fn resumed(&self, state: &SharedState);

    /// Any resource's engine exited. Runs before the host retires it.
    fn resource_exited<'a>(
        &'a self,
        state: &'a SharedState,
        resource: phux_core::ids::ResourceId,
    ) -> LocalBoxFuture<'a, ()>;

    /// Serve a relaunch this kind recognizes as its own, leaving the rest to
    /// the organization path.
    fn claim_relaunch(
        &self,
        ctx: &CommandContext,
        operation: OperationId,
        resource: &ResourceKey,
        expected: Revision,
    ) -> Option<CommandResult>;

    /// Which connection check precedes this kind's handling of `frame`.
    fn frame_admission(&self, frame: &FrameKind) -> FrameAdmission;

    /// Serve a stream or input frame about a resource of this kind: the
    /// `INPUT_*`, `RESIZE_TERMINAL`, `FRAME_ACK` and `HISTORY_REQUEST`
    /// frames. A kind owns no frame it did not opt into: one it does not
    /// serve is refused with `ERROR(WRONG_RESOURCE_KIND)` and the
    /// connection stays.
    fn frame<'a>(
        &'a self,
        ctx: ResourceFrame<'a>,
        frame: FrameKind,
    ) -> LocalBoxFuture<'a, FrameFlow>;

    /// This kind's rows of the `GET_PERF` report.
    fn metrics(&self) -> &'static [phux_perf::Metric];
}

/// The app serving `kind`. Exhaustive: a kind with no app does not compile.
pub(crate) const fn app_for(kind: ResourceKind) -> &'static dyn ResourceApp {
    match kind {
        ResourceKind::Terminal => &terminal::TerminalApp,
        ResourceKind::AgentSession => &agent_session::AgentSessionApp,
        ResourceKind::AccessibilityTree => &document::TREE_APP,
        ResourceKind::PixelSurface => &document::SURFACE_APP,
    }
}

/// Every app this server hosts, for the questions the host asks all of
/// them. `every_app_is_listed` holds it to [`app_for`].
static APPS: [&dyn ResourceApp; 4] = [
    app_for(ResourceKind::Terminal),
    app_for(ResourceKind::AgentSession),
    app_for(ResourceKind::AccessibilityTree),
    app_for(ResourceKind::PixelSurface),
];

/// The build fails if the catalog lists a kind this server does not serve,
/// so [`owning_kind`] cannot lose a facet command to the host arm.
const _: () = {
    let kinds = &phux_protocol::kinds::KINDS;
    let mut row = 0;
    while row < kinds.len() {
        assert!(crate::id_bridge::core_kind(kinds[row].kind).is_some());
        row += 1;
    }
};

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

/// The app serving a local resource, if this host still has its handle.
/// Remote and unknown ids belong to host routing, never a default app.
pub(crate) fn app_serving(
    state: &SharedState,
    wire: &WireResourceId,
) -> Option<&'static dyn ResourceApp> {
    let kind = state.with(|s| {
        s.resource_from_wire(wire)
            .and_then(|core| s.resource_handle(core))
            .map(|handle| handle.kind)
    });
    kind.map(app_for)
}

/// Every app's rows of the `GET_PERF` report.
pub(crate) fn metric_tables() -> Vec<&'static [phux_perf::Metric]> {
    APPS.iter().map(|app| app.metrics()).collect()
}

/// The native bootstrap profiles this server can offer at HELLO.
pub(crate) fn bootstrap_capabilities() -> phux_protocol::caps::BootstrapCapabilities {
    terminal::bootstrap_capabilities()
}

/// The app that owns `command` in the catalog; `None` for a host command.
pub(crate) fn app_owning(command: &Command) -> Option<&'static dyn ResourceApp> {
    owning_kind(command).map(app_for)
}

/// The kind whose catalog rows carry `command`; `None` for a host command.
fn owning_kind(command: &Command) -> Option<ResourceKind> {
    phux_protocol::kinds::command_kind(command).and_then(crate::id_bridge::core_kind)
}

#[cfg(test)]
mod seam_tests;
#[cfg(test)]
mod tests;
