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

use futures_util::future::LocalBoxFuture;
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::ids::{OperationId, ResourceId as WireResourceId, ResourceKey, Revision};
use phux_protocol::wire::frame::{Command, CommandResult, FrameKind, ViewportInfo};
use tokio_util::sync::CancellationToken;

use super::commands::{AttachResourceFailure, CommandContext};
use super::connections::dispatch::FrameFlow;
use super::operation_dedupe::OperationClaim;
use super::standing::MirrorSource;
use crate::resource::{ResourceHandle, ResourceKind};
use crate::state::{ClientId, ServerState, SharedState};

pub(crate) mod agent_session;
pub(crate) mod agent_tracking;
mod context;
pub(crate) mod document;
pub(crate) mod lane;
pub(crate) mod mirror_demand;
pub(crate) mod spawn;
pub(crate) mod terminal;
pub(crate) mod upgrade;

pub(crate) use context::{
    FrameAdmission, ResourceFrame, ResourceLaunch, SessionSeed, StreamBootstrap,
};
pub(crate) use lane::{ConnectionLane, InputLane, RunningLane};
pub(crate) use spawn::ResourceSpawn;
pub(crate) use upgrade::{KindResume, UpgradeCut};

/// What the host asks of the app serving a kind.
pub(crate) trait ResourceApp: Sync {
    /// Whether ordinary organization creation/relaunch can execute this kind.
    /// Producer-bound projections instead open through their producer's app.
    fn supports_organization_launch(&self) -> bool;

    /// Execute a committed launch. A failure is reported against the effect's
    /// activation by the host; it cannot undo the accepted operation.
    fn launch(&self, launch: ResourceLaunch<'_>) -> Result<(), String>;

    /// Stage this kind's bounded session bootstrap and a parked live pump.
    /// Nothing may publish until the host opens the aggregate live gate.
    /// A cursor-driven app may defer its stream to explicit `ATTACH_RESOURCE`;
    /// returning no frames does not close or remove its catalog membership.
    fn stage_session<'a>(
        &'a self,
        capture: super::session_attach::SessionCapture<'a>,
        staging: &'a mut super::session_attach::staging::SessionStaging,
        pane: crate::state::AttachSnapshotPane,
    ) -> LocalBoxFuture<'a, Result<(), super::session_attach::staging::CaptureFailure>>;

    /// Before a session `ATTACH` reads membership: refresh what its snapshot
    /// reports of this kind's resources (a Terminal's live cwd).
    fn refresh_for_attach<'a>(&'a self, state: &'a SharedState) -> LocalBoxFuture<'a, ()>;

    /// A session `ATTACH` is about to stage `panes`, all of this kind:
    /// fence the client's earlier consumers and apply its presentation.
    fn prepare_attach<'a>(
        &'a self,
        attach: super::session_attach::SessionAttach<'a>,
        panes: &'a [crate::state::AttachSnapshotPane],
    ) -> LocalBoxFuture<'a, ()>;

    /// Size `panes`, all of this kind, to the attaching client's viewport,
    /// in the lock that records it.
    fn attach_viewport(
        &self,
        s: &mut crate::state::ServerState,
        panes: &[crate::state::AttachSnapshotPane],
        viewport: phux_protocol::wire::frame::ViewportInfo,
    );

    /// The session `ATTACH` published: apply its declared role to `panes`,
    /// all of this kind.
    fn after_attach<'a>(
        &'a self,
        attach: super::session_attach::SessionAttach<'a>,
        panes: &'a [crate::state::AttachSnapshotPane],
        role: phux_protocol::wire::frame::RolePolicy,
    ) -> LocalBoxFuture<'a, ()>;

    /// The session `ATTACH` failed before publication: release `client`'s
    /// engine-side consumers of `handles`, all of this kind, each within a
    /// bounded wait.
    fn abort_attach<'a>(
        &'a self,
        client: ClientId,
        handles: &'a [ResourceHandle],
    ) -> LocalBoxFuture<'a, ()>;

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

    /// Begin carrying this kind's resources across a graceful upgrade;
    /// `None` while another upgrade holds them.
    fn begin_upgrade(&self, state: &SharedState) -> Option<Box<dyn UpgradeCut>>;

    /// This kind's half of resuming from an upgrade blob, made before the
    /// rebuild takes the state lock.
    fn resume(&self, state: &SharedState) -> Box<dyn KindResume>;

    /// The tree came back from an upgrade blob and no client is connected.
    fn resumed(&self, state: &SharedState);

    /// Any resource's engine exited. Runs before the host retires it.
    fn resource_exited<'a>(
        &'a self,
        state: &'a SharedState,
        resource: phux_core::ids::ResourceId,
    ) -> LocalBoxFuture<'a, ()>;

    /// A resource of this kind exited and is retained (ADR-0124): withdraw
    /// what its exit invalidates, in the lock that retains it, after its
    /// `Exited` event. Returns a hook the withdrawal owes, fired off-lock.
    fn exit_retained(
        &self,
        s: &mut crate::state::ServerState,
        wire: &WireResourceId,
    ) -> Option<crate::hooks::HookEvent>;

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

    /// This kind's tables of the `GET_PERF` report, its engine's included.
    fn metrics(&self, state: &SharedState) -> Vec<&'static [phux_perf::Metric]>;

    /// The server is starting: start what this kind runs for its lifetime,
    /// and return the server's input lane (ADR-0044) if this kind runs it.
    fn start(&self, state: &SharedState) -> std::io::Result<Option<RunningLane>>;

    /// The native bootstrap profiles this kind lets `HELLO` offer, if any.
    fn bootstrap_capabilities(&self, state: &SharedState) -> Option<BootstrapCapabilities>;

    /// Serve a session `ATTACH`'s `CreateIfMissing` with this kind's default
    /// resource, if this kind seeds sessions; `Err` hands `seed` back. The
    /// future yields the session's name, or `None` once a refusal is sent.
    fn seed_session<'a>(
        &'a self,
        seed: SessionSeed<'a>,
    ) -> Result<LocalBoxFuture<'a, Option<String>>, SessionSeed<'a>>;

    /// A connection's `VIEWPORT_RESIZE`: its size vote for every resource
    /// of this kind it is attached to.
    fn viewport_resize(&self, state: &SharedState, client: ClientId, viewport: &ViewportInfo);

    /// A connection used one of its views, with structured input to any
    /// resource, local or not.
    fn client_activity(&self, state: &SharedState, client: ClientId);

    /// The command guard refused `command` with `result`.
    fn command_refused(
        &self,
        s: &ServerState,
        client: ClientId,
        command: &Command,
        result: &CommandResult,
    );

    /// Keep the standing mirrors and producers `source` names running, if
    /// this kind runs them; says whether it does.
    fn keep_standing(
        &self,
        state: &SharedState,
        source: &MirrorSource,
        root_token: &CancellationToken,
    ) -> bool;
}

/// The refusal a stream that needs the connection's Terminal bootstrap
/// profile (its kind's catalog stream contract says so) gets on a connection
/// whose `HELLO` offered none. The connection stays.
pub(crate) const NO_TERMINAL_PROFILE: &str = "this connection offered no Terminal bootstrap profile at HELLO, so it cannot consume a Terminal stream";

/// The app serving `kind`. Exhaustive: a kind with no app does not compile.
pub(crate) const fn app_for(kind: ResourceKind) -> &'static dyn ResourceApp {
    match kind {
        ResourceKind::Terminal => &terminal::TerminalApp,
        ResourceKind::AgentSession => &agent_session::AgentSessionApp,
        ResourceKind::AccessibilityTree => &document::TREE_APP,
        ResourceKind::PixelSurface => &document::SURFACE_APP,
        ResourceKind::Document => &document::DOCUMENT_APP,
    }
}

/// The kind of each [`APPS`] entry, in order. A parent's kind comes before
/// its children's: an upgrade resume rebuilds in this order.
const APP_KINDS: [ResourceKind; 5] = [
    ResourceKind::Terminal,
    ResourceKind::AgentSession,
    ResourceKind::AccessibilityTree,
    ResourceKind::PixelSurface,
    ResourceKind::Document,
];

/// Every app this server hosts, for the questions the host asks all of
/// them. `every_app_is_listed` holds [`APP_KINDS`] to the catalog.
static APPS: [&dyn ResourceApp; 5] = [
    app_for(APP_KINDS[0]),
    app_for(APP_KINDS[1]),
    app_for(APP_KINDS[2]),
    app_for(APP_KINDS[3]),
    app_for(APP_KINDS[4]),
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

/// `items` split by kind, each with the app serving it, in [`APP_KINDS`]
/// order; a kind with none is left out.
pub(crate) fn by_kind<T: Clone>(
    items: &[T],
    kind_of: impl Fn(&T) -> ResourceKind,
) -> Vec<(&'static dyn ResourceApp, Vec<T>)> {
    APP_KINDS
        .iter()
        .filter_map(|&kind| {
            let of_kind: Vec<T> = items
                .iter()
                .filter(|item| kind_of(item) == kind)
                .cloned()
                .collect();
            (!of_kind.is_empty()).then(|| (app_for(kind), of_kind))
        })
        .collect()
}

/// Ask every app to refresh what a session `ATTACH` snapshot reports.
pub(crate) async fn refresh_for_attach(state: &SharedState) {
    for app in APPS {
        app.refresh_for_attach(state).await;
    }
}

/// Seed `seed`'s session with the default resource of the app that seeds
/// sessions; `None` when no app does.
pub(crate) fn seed_session(seed: SessionSeed<'_>) -> Option<LocalBoxFuture<'_, Option<String>>> {
    let mut seed = seed;
    for app in APPS {
        match app.seed_session(seed) {
            Ok(seeding) => return Some(seeding),
            Err(declined) => seed = declined,
        }
    }
    None
}

/// Start every app for the server's lifetime; the server's input lane, when
/// an app runs one.
pub(crate) fn start(state: &SharedState) -> std::io::Result<Option<RunningLane>> {
    let mut input_lane = None;
    for app in APPS {
        if let Some(lane) = app.start(state)? {
            debug_assert!(input_lane.is_none(), "the server runs one input lane");
            input_lane.get_or_insert(lane);
        }
    }
    Ok(input_lane)
}

/// Tell every app a connection's `VIEWPORT_RESIZE`.
pub(crate) fn viewport_resize(state: &SharedState, client: ClientId, viewport: &ViewportInfo) {
    for app in APPS {
        app.viewport_resize(state, client, viewport);
    }
}

/// Tell every app a connection used one of its views.
pub(crate) fn client_activity(state: &SharedState, client: ClientId) {
    for app in APPS {
        app.client_activity(state, client);
    }
}

/// Tell every app the command guard refused `command`.
pub(crate) fn command_refused(
    s: &ServerState,
    client: ClientId,
    command: &Command,
    result: &CommandResult,
) {
    for app in APPS {
        app.command_refused(s, client, command, result);
    }
}

/// Keep the standing entries `source` names running, in the app that runs
/// them.
pub(crate) fn keep_standing(
    state: &SharedState,
    source: Option<MirrorSource>,
    root_token: &CancellationToken,
) {
    let Some(source) = source else {
        return;
    };
    if !APPS
        .iter()
        .any(|app| app.keep_standing(state, &source, root_token))
    {
        tracing::warn!("no app runs standing mirrors; [[mirrors]] and [[producers]] are ignored");
    }
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

/// Every app's tables of the `GET_PERF` report.
pub(crate) fn metric_tables(state: &SharedState) -> Vec<&'static [phux_perf::Metric]> {
    APPS.iter().flat_map(|app| app.metrics(state)).collect()
}

/// The native bootstrap profiles this server can offer at HELLO.
pub(crate) fn bootstrap_capabilities(state: &SharedState) -> BootstrapCapabilities {
    APPS.iter()
        .find_map(|app| app.bootstrap_capabilities(state))
        .unwrap_or_default()
}

/// The app that owns `command` in the catalog; `None` for a host command.
pub(crate) fn app_owning(command: &Command) -> Option<&'static dyn ResourceApp> {
    owning_kind(command).map(app_for)
}

/// The kind whose catalog rows carry `command`, or that serves the
/// host-addressed method carrying it; `None` for a host command.
fn owning_kind(command: &Command) -> Option<ResourceKind> {
    phux_protocol::kinds::command_served_by(command).and_then(crate::id_bridge::core_kind)
}

#[cfg(test)]
mod seam_ratchet_tests;
#[cfg(test)]
mod seam_tests;
#[cfg(test)]
mod tests;
