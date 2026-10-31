//! The apps this server hosts, one per resource kind.
//!
//! An app owns everything its kind needs from the host: its facet commands
//! and how a client's stream bootstraps. Host modules reach a kind only
//! through [`app_for`] and [`app_owning`], so what a kind *is* stays in its
//! app, and a new kind is one [`ResourceApp`] plus one [`APPS`] row.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared across the runtime; pub would violate unreachable_pub"
)]

use futures_util::future::LocalBoxFuture;
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::ids::StreamId;
use phux_protocol::wire::frame::{Command, CommandResult};
use tokio_util::sync::CancellationToken;

use super::commands::{AttachResourceFailure, CommandContext};
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
    fn check_takeover(&self) -> Result<(), crate::resource::WrongResourceKind> {
        Ok(())
    }

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

    /// Ask this resource to adopt `cols` by `rows`; whether it took the
    /// request. A kind without geometry never does.
    fn resize(
        &self,
        _handle: &ResourceHandle,
        _size: (u16, u16),
        _cell_px: Option<(u16, u16)>,
    ) -> bool {
        false
    }

    /// Where this resource answers a graceful-upgrade handoff request.
    fn upgrade_sender(
        &self,
        _handle: &ResourceHandle,
    ) -> Option<tokio::sync::mpsc::Sender<crate::resource::handoff::UpgradeHandleRequest>> {
        None
    }

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
}

/// Every app this server hosts.
static APPS: [&dyn ResourceApp; 2] = [&terminal::TerminalApp, &agent_session::AgentSessionApp];

/// The app serving `kind`, or `None` for a kind this build does not serve.
pub(crate) fn app_for(kind: ResourceKind) -> Option<&'static dyn ResourceApp> {
    APPS.iter().copied().find(|app| app.kind() == kind)
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
