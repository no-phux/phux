//! The apps this server hosts, one per resource kind.
//!
//! An app owns everything its kind needs from the host: its facet commands,
//! the input and stream frames addressed to its resources, and how a
//! client's stream bootstraps. The runtime reaches a kind only through
//! [`app_for`], [`app_owning`] and [`app_addressed`], so what a kind *is*
//! stays in its app, and a new kind is one [`ResourceApp`] plus one [`APPS`]
//! row. `state` sits below this module: what it needs from any resource is
//! [`crate::resource::ResourceHandle::ops`].

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared across the runtime; pub would violate unreachable_pub"
)]

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::ids::{ResourceId as WireResourceId, StreamId};
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};
use tokio_util::sync::CancellationToken;

use super::commands::{AttachResourceFailure, CommandContext};
use super::connections::dispatch::FrameFlow;
use super::connections::hello::NegotiatedConnection;
use super::connections::plumbing::ClientPlumbing;
use super::input_lane::{InputCredits, InputLaneHandle};
use super::operation_dedupe::OperationClaim;
use crate::resource::{ResourceHandle, ResourceKind};
use crate::state::{ClientId, Outbound, SharedState};

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

/// The connection that read an input or stream frame, lent to the app
/// serving the resource the frame addresses.
pub(in crate::runtime) struct StreamFrameContext<'a> {
    /// Server state.
    pub(in crate::runtime) state: &'a SharedState,
    /// The sending connection.
    pub(in crate::runtime) client_id: ClientId,
    /// The connection's cancellation.
    pub(in crate::runtime) token: &'a CancellationToken,
    /// The server's input lane, when it runs one.
    pub(in crate::runtime) input_lane: Option<&'a InputLaneHandle>,
    /// The connection's outbound mailboxes, bound streams included.
    pub(in crate::runtime) plumbing: &'a ClientPlumbing,
    /// What HELLO negotiated.
    pub(in crate::runtime) negotiated: Option<NegotiatedConnection>,
    /// The connection's pane input credits.
    pub(in crate::runtime) input_credits: &'a mut InputCredits,
}

/// What the host asks of the app serving a kind.
pub(in crate::runtime) trait ResourceApp: Sync {
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

    /// Serve an input or stream frame addressed to a resource of this kind
    /// (see [`addressed_resource`]). A kind that serves none refuses each
    /// with an uncorrelated `WRONG_RESOURCE_KIND` and keeps the connection.
    fn stream_frame<'a>(
        &'a self,
        ctx: StreamFrameContext<'a>,
        frame: FrameKind,
    ) -> LocalBoxFuture<'a, FrameFlow> {
        let message = format!(
            "frame type {:#04x} is not valid on a {} stream",
            frame.type_byte(),
            self.kind().as_str()
        );
        async move {
            super::send_error(&ctx.plumbing.out_tx, ErrorCode::WrongResourceKind, &message).await;
            FrameFlow::Continue(())
        }
        .boxed_local()
    }

    /// Bootstrap one client's stream onto a resource of this kind.
    fn bootstrap<'a>(
        &'a self,
        stream: StreamBootstrap<'a>,
    ) -> LocalBoxFuture<'a, Result<(), AttachResourceFailure>>;
}

/// Every app this server hosts.
static APPS: [&dyn ResourceApp; 2] = [&terminal::TerminalApp, &agent_session::AgentSessionApp];

/// The app serving `kind`, or `None` for a kind this build does not serve.
pub(in crate::runtime) fn app_for(kind: ResourceKind) -> Option<&'static dyn ResourceApp> {
    APPS.iter().copied().find(|app| app.kind() == kind)
}

/// The native bootstrap profiles this server can offer at HELLO.
pub(crate) fn bootstrap_capabilities() -> phux_protocol::caps::BootstrapCapabilities {
    terminal::bootstrap_capabilities()
}

/// The app that owns `command` in the catalog; `None` for a host command.
pub(in crate::runtime) fn app_owning(command: &Command) -> Option<&'static dyn ResourceApp> {
    app_for(crate::resource::core_kind(
        phux_protocol::kinds::command_kind(command)?,
    )?)
}

/// The resource an input or stream frame addresses; `None` for a frame
/// that is not about one resource's stream.
pub(in crate::runtime) const fn addressed_resource(frame: &FrameKind) -> Option<&WireResourceId> {
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

/// The app serving the resource `id` names. An id that resolves to no local
/// resource (a satellite's, or none) takes the Terminal path, whose handlers
/// relay or drop it.
pub(in crate::runtime) fn app_addressed(
    state: &SharedState,
    id: &WireResourceId,
) -> &'static dyn ResourceApp {
    state
        .with(|s| {
            s.resource_from_wire(id)
                .and_then(|core| s.resource_handle(core))
                .map(|handle| handle.kind)
        })
        .and_then(app_for)
        .unwrap_or(&terminal::TerminalApp)
}

#[cfg(test)]
mod seam_tests;
#[cfg(test)]
mod tests;
