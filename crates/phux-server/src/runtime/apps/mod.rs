//! Kind-owned stream capture and engine facts. Host command, spawn and
//! upgrade operation owners retain policy, registration and evidence.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared across the runtime; pub would violate unreachable_pub"
)]

use futures_util::future::LocalBoxFuture;
use phux_protocol::wire::frame::FrameKind;

use super::commands::AttachResourceFailure;
use crate::resource::ResourceKind;

pub(crate) mod agent_session;
pub(crate) mod agent_tracking;
mod context;
mod stream_host;
pub(crate) use stream_host::{StreamHost, StreamLease, StreamReservation};
pub(crate) mod document;
pub(crate) mod mirror_demand;
pub(crate) mod producer_stream;
pub(crate) mod terminal;
pub(crate) use context::{FrameAdmission, StreamBootstrap};
pub(crate) use terminal::input_lane::{ConnectionLane, InputLaneHandle as InputLane};

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

    /// Bootstrap one client's stream onto a resource of this kind.
    fn bootstrap<'a>(
        &'a self,
        stream: StreamBootstrap<'a>,
    ) -> LocalBoxFuture<'a, Result<(), AttachResourceFailure>>;

    /// Which connection check precedes this kind's handling of `frame`.
    fn frame_admission(&self, frame: &FrameKind) -> FrameAdmission;

    /// Kind-owned frame validity facts. `None` identifies Terminal engine
    /// frames whose admitted routing is owned by the host.
    fn frame_error(
        &self,
        frame: &FrameKind,
    ) -> Option<(phux_protocol::wire::frame::ErrorCode, String)>;
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

/// Every catalog kind has a stream app.
const _: () = {
    let kinds = &phux_protocol::kinds::KINDS;
    let mut row = 0;
    while row < kinds.len() {
        assert!(crate::id_bridge::core_kind(kinds[row].kind).is_some());
        row += 1;
    }
};

/// Whether this kind owns an engine for organization launch effects.
pub(crate) const fn supports_organization_launch(kind: ResourceKind) -> bool {
    matches!(kind, ResourceKind::Terminal)
}

/// Terminal streams on a multi-stream connection wait for `STREAM_BIND`.
pub(crate) const fn defers_stream_bootstrap(kind: ResourceKind) -> bool {
    matches!(kind, ResourceKind::Terminal)
}

/// Whether a session `ATTACH` itself publishes this kind's stream generation.
/// Agent sessions wait for an explicit cursor; a deferred Terminal for
/// `STREAM_BIND`. Either later request opens the first generation.
pub(crate) const fn publishes_session_stream(kind: ResourceKind, deferred: bool) -> bool {
    match kind {
        ResourceKind::AgentSession => false,
        kind => !(deferred && defers_stream_bootstrap(kind)),
    }
}
