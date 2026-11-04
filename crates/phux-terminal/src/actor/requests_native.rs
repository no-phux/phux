use super::{BootstrapId, Outbound, ResourceOutput, StreamId, mpsc, oneshot};
use bytes::Bytes;
use phux_protocol::wire::frame::FrameKind;
use tokio::sync::broadcast;

/// Fully-owned native prefix captured atomically by the terminal actor.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativeBootstrapReply {
    /// Bounded BEGIN/CHUNK/READY sequence for the per-client pump to publish.
    pub frames: Vec<FrameKind>,
    /// Heap retained by the opaque payloads, charged to the connection's
    /// bootstrap staging budget.
    pub retained_bytes: usize,
    /// Actor-global raw output cut included by the checkpoint.
    pub base_seq: u64,
    /// Actor-private generation key used only for the publication fence.
    pub publication_cursor: crate::native_state::OpaqueHistoryCursor,
}

/// Live bytes after a native checkpoint fence, plus the receiver installed
/// atomically after the replay cut.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativePublicationReply {
    pub replay: Vec<(u64, Bytes)>,
    pub live: broadcast::Receiver<ResourceOutput>,
}

/// Complete the `READY`/`ATTACH_READY` publication fence for one owner.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativePublicationRequest {
    /// Server-local lease owner completing publication.
    pub owner: u64,
    /// Wire terminal identity for the published replica.
    pub terminal_id: phux_protocol::ids::ResourceId,
    /// Logical stream identity for the published replica.
    pub stream_id: StreamId,
    /// Replica generation whose READY frame was just sent.
    pub bootstrap_id: BootstrapId,
    /// Authenticated actor-private generation cursor returned with READY.
    pub cursor: crate::native_state::OpaqueHistoryCursor,
    /// Completion carrying replay bytes and the post-replay live receiver.
    pub reply:
        oneshot::Sender<Result<NativePublicationReply, crate::native_state::NativeStateError>>,
}

/// Native checkpoint capture request serialized by the terminal actor.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativeBootstrapRequest {
    /// Server-local owner identity used for detach and TTL cleanup.
    pub owner: u64,
    /// Wire terminal identity for this subscription.
    pub terminal_id: phux_protocol::ids::ResourceId,
    /// Logical stream identity.
    pub stream_id: StreamId,
    /// Replica generation identity.
    pub bootstrap_id: BootstrapId,
    /// Negotiated payload limits.
    pub limits: phux_protocol::caps::BootstrapLimits,
    /// Remaining connection-wide opaque byte budget.
    pub max_bytes: usize,
    /// Remaining connection-wide frame budget.
    pub max_frames: usize,
    /// Atomic capture result. The pump alone publishes the returned frames.
    pub reply: oneshot::Sender<Result<NativeBootstrapReply, crate::native_state::NativeStateError>>,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
impl NativeBootstrapRequest {
    /// A request granted the whole native budget: every prefix byte and
    /// chunk the engine may retain, plus the BEGIN and READY frames.
    #[must_use]
    pub const fn full_budget(
        owner: u64,
        terminal_id: phux_protocol::ids::ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        limits: phux_protocol::caps::BootstrapLimits,
        reply: oneshot::Sender<Result<NativeBootstrapReply, crate::native_state::NativeStateError>>,
    ) -> Self {
        Self {
            owner,
            terminal_id,
            stream_id,
            bootstrap_id,
            limits,
            max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
            max_frames: crate::native_state::MAX_NATIVE_BOOTSTRAP_FRAMES,
            reply,
        }
    }
}

/// Actor result paired with the caller's still-owned outbound permit.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativeHistoryReply {
    /// Permit proving mailbox capacity was reserved before actor advancement.
    pub permit: mpsc::OwnedPermit<Outbound>,
    /// Fully-owned response frame, or an invalid request/host failure.
    pub result: Result<FrameKind, crate::native_state::NativeStateError>,
}

/// One bounded native history step routed to the owning terminal actor.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativeHistoryRequest {
    /// Reserved client-mailbox capacity. The actor returns but never consumes it.
    pub permit: mpsc::OwnedPermit<Outbound>,
    /// Server-local owner identity authenticated against the retained cut.
    pub owner: u64,
    /// Wire terminal identity for this subscription.
    pub terminal_id: phux_protocol::ids::ResourceId,
    /// Logical stream identity.
    pub stream_id: StreamId,
    /// Replica generation identity.
    pub bootstrap_id: BootstrapId,
    /// Stable opaque capability echoed by the client.
    pub cursor: Bytes,
    /// Requested non-zero response byte bound.
    pub max_bytes: u32,
    /// Requested non-zero history row bound.
    pub max_rows: u32,
    /// Negotiated connection bounds.
    pub limits: phux_protocol::caps::BootstrapLimits,
    /// Actor result; the pump consumes the returned permit.
    pub reply: oneshot::Sender<NativeHistoryReply>,
}
/// Release every retained native history cut owned by one detached client.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativeReleaseRequest {
    /// Server-local owner identity.
    pub owner: u64,
}
