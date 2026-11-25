//! What the host hands an app with each [`super::ResourceApp`] call.

use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::ids::StreamId;

use crate::resource::ResourceHandle;
use crate::state::{ClientId, Outbound};

/// One client's stream onto one resource, ready to bootstrap.
#[derive(Debug)]
pub(crate) struct StreamBootstrap<'a> {
    /// Reserved by the host before the first bootstrap await.
    pub(crate) reservation: super::StreamReservation,
    /// The attaching connection.
    pub(crate) client_id: ClientId,
    /// The resource's wire id.
    pub(crate) wire: &'a phux_protocol::ids::ResourceId,
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
    /// A replay cursor, for kinds that resume from one.
    pub(crate) after_seq: Option<u64>,
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
