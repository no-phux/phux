//! The apps this server hosts, one per resource kind. The runtime routes
//! per-kind commands, frames, streams and upgrades through [`ResourceApp`].
//! Services with a fixed owner have their own entry points. `state` sits
//! below the apps.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared across the runtime; pub would violate unreachable_pub"
)]

use futures_util::future::LocalBoxFuture;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, FrameKind};

use super::commands::{AttachResourceFailure, CommandContext};
use super::connections::dispatch::FrameFlow;
use super::operation_dedupe::OperationClaim;
use crate::resource::ResourceKind;
use crate::state::SharedState;

pub(crate) mod agent_session;
pub(crate) mod agent_tracking;
mod context;
pub(crate) mod document;
pub(crate) mod mirror_demand;
mod producer_stream;
pub(crate) mod spawn;
pub(crate) mod terminal;
pub(crate) mod upgrade;

pub(crate) use agent_session::{
    claim_relaunch as claim_native_agent_relaunch,
    retry_pending_closures as retry_pending_agent_closures,
    source_exited as close_native_agent_sources,
};
pub(crate) use context::{
    FrameAdmission, ResourceFrame, ResourceLaunch, SessionSeed, StreamBootstrap,
};
pub(crate) use spawn::ResourceSpawn;
pub(crate) use terminal::input_lane::{ConnectionLane, InputLaneHandle as InputLane};
pub(crate) use upgrade::{KindResume, UpgradeCut};

/// What the host asks of the app serving a kind.
pub(crate) trait ResourceApp: Sync {
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

/// The kinds hosted in upgrade order, with parents before their children.
const APP_KINDS: [ResourceKind; 5] = [
    ResourceKind::Terminal,
    ResourceKind::AgentSession,
    ResourceKind::AccessibilityTree,
    ResourceKind::PixelSurface,
    ResourceKind::Document,
];

/// Every catalog kind has a server app, so [`app_owning`] cannot lose a
/// facet command to the host arm.
const _: () = {
    let kinds = &phux_protocol::kinds::KINDS;
    let mut row = 0;
    while row < kinds.len() {
        assert!(crate::id_bridge::core_kind(kinds[row].kind).is_some());
        row += 1;
    }
};

/// The launcher for a committed organization effect. Producer-bound kinds
/// open through their producer, so they have no organization launcher.
pub(crate) const fn organization_launcher(
    kind: ResourceKind,
) -> Option<fn(ResourceLaunch<'_>) -> Result<(), String>> {
    match kind {
        ResourceKind::Terminal => Some(terminal::launch::execute),
        ResourceKind::AgentSession
        | ResourceKind::AccessibilityTree
        | ResourceKind::PixelSurface
        | ResourceKind::Document => None,
    }
}

/// Terminal streams on a multi-stream connection wait for `STREAM_BIND`.
pub(crate) const fn defers_stream_bootstrap(kind: ResourceKind) -> bool {
    matches!(kind, ResourceKind::Terminal)
}

/// The app serving a local resource, if this host still has its handle.
/// Remote and unknown ids belong to host routing, never a default app.
pub(crate) fn app_serving(
    state: &SharedState,
    wire: &WireResourceId,
) -> Option<&'static dyn ResourceApp> {
    let kind = state.with(|s| {
        s.idspace
            .resource_from_wire(wire)
            .and_then(|core| s.resources().handle(core))
            .map(|handle| handle.kind)
    });
    kind.map(app_for)
}

/// The app that owns `command` in the catalog; `None` for a host command.
pub(crate) fn app_owning(command: &Command) -> Option<&'static dyn ResourceApp> {
    phux_protocol::kinds::command_served_by(command)
        .and_then(crate::id_bridge::core_kind)
        .map(app_for)
}

#[cfg(test)]
mod seam_tests;
#[cfg(test)]
mod tests;
