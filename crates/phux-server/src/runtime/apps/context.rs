//! What the host hands an app with each [`super::ResourceApp`] call.

use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::ids::StreamId;
use tokio_util::sync::CancellationToken;

use super::lane::ConnectionLane;
use crate::resource::ResourceHandle;
use crate::runtime::connections::hello::NegotiatedConnection;
use crate::runtime::connections::plumbing::ClientPlumbing;
use crate::state::{ClientId, Outbound, SharedState};

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

/// One already-committed launch effect. The app must execute this exact
/// resource activation, never substitute another kind's default launch.
#[derive(Clone, Copy)]
pub(crate) struct ResourceLaunch<'a> {
    pub(crate) state: &'a SharedState,
    pub(crate) resource: phux_core::ids::ResourceId,
    pub(crate) spec: &'a phux_core::LaunchSpec,
    pub(crate) root_token: &'a CancellationToken,
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
    /// The connection's place on the input lane, when it has one.
    pub(crate) lane: Option<&'a mut ConnectionLane>,
    /// The connection's mailboxes: control, and a bound stream per resource.
    pub(crate) plumbing: &'a ClientPlumbing,
    /// What `HELLO` negotiated, once it has.
    pub(crate) negotiated: Option<NegotiatedConnection>,
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

/// A session `ATTACH`'s `CreateIfMissing` for a session that may not exist:
/// what the app that seeds one needs to create it.
pub(crate) struct SessionSeed<'a> {
    pub(crate) state: &'a SharedState,
    /// The session to attach, created when it does not exist.
    pub(crate) name: String,
    /// The wire's command for the seed, if any.
    pub(crate) command: Option<Vec<String>>,
    /// The wire's working directory for the seed, if any.
    pub(crate) cwd: Option<String>,
    /// Where a refusal goes.
    pub(crate) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    pub(crate) root_token: &'a CancellationToken,
    /// The attaching client's default colors.
    pub(crate) default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
}
