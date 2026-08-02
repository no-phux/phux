//! Submodule for terminal actor internals.

use std::os::fd::RawFd;

use crate::grid::SnapshotBytes;
use crate::state::{Outbound, TerminalInput};
use bytes::Bytes;
use phux_protocol::ClientId;
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::wire::frame::{ControlAction, FrameKind, TerminalEventType, TerminalSignal};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

/// A supervisory control request delivered to a [`super::TerminalActor`] over
/// its `control` mailbox (ADR-0033, "take the wheel + kill").
///
/// The input *lease* itself lives in [`crate::state::ServerState`] (the input
/// gate runs there, under the state lock, where the originating `ClientId` is
/// known). The actor is the emitter of the
/// [`phux_protocol::wire::frame::AgentEvent::TerminalControl`] broadcast
/// because it owns both the event-subscriber list and the process lifecycle —
/// so the handler forwards the *fact* of a change and lets the actor stamp its
/// current lifecycle and fan the event out.
#[derive(Debug)]
pub enum ControlRequest {
    /// The input lease changed in `ServerState`; emit a `TerminalControl`
    /// broadcast reflecting the new holder and the action that produced it.
    LeaseChanged {
        /// The client now holding the lease, or `None` if released to `Open`.
        input_holder: Option<ClientId>,
        /// What just happened (`Acquired` / `Seized` / `Released`).
        action: ControlAction,
        /// The client that performed the action.
        actor: ClientId,
    },
    /// Something other than the detector wrote this pane's `phux.agent/v1`
    /// record (an explicit `SET_METADATA` or `DELETE_METADATA`), so the
    /// detector's edge filter — a model of its own emissions — is now a model
    /// of a store that no longer exists (ADR-0046 §E).
    ///
    /// The actor clears the filter, re-arming exactly one republish on the
    /// next tick. This is what makes `DELETE` mean "the detector resumes"
    /// rather than "the record vanishes until the agent's state happens to
    /// change" — which, for an agent sitting idle waiting on a human, is
    /// never. No-op on a pane with no detector.
    AgentRecordInvalidated,
    /// Deliver `signal` to the pane's process group, update the lifecycle
    /// (`Freeze` → `Frozen`, `Resume` → `Running`), and broadcast a
    /// `TerminalControl`. `reply` carries `Ok(())` on delivery or a
    /// human-readable error (no PTY / no pid / `killpg` failed).
    Signal {
        /// The signal to deliver.
        signal: TerminalSignal,
        /// The lease holder at the time of the signal, for the broadcast.
        input_holder: Option<ClientId>,
        /// The client requesting the signal.
        by: ClientId,
        /// Delivery acknowledgement.
        reply: oneshot::Sender<Result<(), String>>,
    },
}

/// Request to register a new consumer with the actor.
///
/// Drives the ADR-0018 per-consumer state lifecycle. The caller is the
/// runtime's ATTACH path, which has just installed the client in
/// `ServerState`.
///
/// For state-sync consumers the actor synthesizes the bootstrap snapshot,
/// primes the per-consumer reference from that exact canonical terminal cut,
/// installs the lifecycle entry, and returns both snapshot and cut in one
/// reply. No PTY event can run between those operations. Raw consumers retain
/// the lightweight lifecycle-only path and return no synthesized bootstrap.
#[derive(Debug)]
pub struct ConsumerAttachRequest {
    /// Identifier the actor will key the per-consumer state by. Must
    /// match the `ClientId` the caller uses in subsequent
    /// `ConsumerDetachRequest`s and `FRAME_ACK` routing.
    pub client_id: ClientId,
    /// Per-consumer outbound mailbox. The actor stores a clone in the
    /// per-consumer [`super::ConsumerSyncState`] and uses it on every tick
    /// (phux-q0e.3) to push a `TerminalOutput` frame carrying the
    /// incremental synthesis bytes.
    pub outbound: mpsc::Sender<Outbound>,
    /// Wire-level terminal id (`u32`). The actor stamps it on every
    /// emitted `TerminalOutput` frame. The runtime owns the mapping
    /// from the actor's [`phux_core::ids::TerminalId`] to this wire id and
    /// passes the resolved value here at ATTACH time.
    pub wire_terminal_id: u32,
    /// Logical protocol-0.7 subscription identity.
    pub stream_id: StreamId,
    /// Current replica generation for this subscription.
    pub bootstrap_id: BootstrapId,
    /// Whether this consumer negotiated the synthesized state-sync tick
    /// emitter (`OutputMode::StateSync`) at HELLO time (phux-fseo). When
    /// `true` the actor's `tick_emit` serves this consumer and the runtime
    /// suppresses its broadcast pump for it; when `false` the consumer
    /// stays on the raw PTY broadcast (the human-TUI default).
    pub wants_state_sync: bool,
    /// Requested synthesized scrollback for the atomic state-sync bootstrap.
    ///
    /// Ignored when [`Self::wants_state_sync`] is false. `None` means the
    /// state-sync snapshot contains only the active screen.
    pub state_sync_scrollback: Option<u32>,
    /// Maximum snapshot bytes this registration may allocate before replying.
    pub bootstrap_max_bytes: usize,
    /// Maximum bootstrap frames the caller can still retain.
    pub bootstrap_max_frames: usize,
    /// Negotiated maximum bytes per synthesized bootstrap chunk.
    pub bootstrap_chunk_bytes: usize,
    /// Whether this consumer is on a lossy/forwarded leg and should use the
    /// advance-on-ack loss-tolerant emission model (phux-v45.8, ADR-0042).
    ///
    /// Only meaningful together with `wants_state_sync` (a raw broadcast-pump
    /// consumer has no per-consumer reference to make loss-tolerant). When
    /// `true` the actor enables loss-tolerance right after registration: the
    /// per-consumer reference then advances on `FRAME_ACK` rather than on emit,
    /// so a frame the forwarded leg drops re-diffs against the last-acked
    /// reference and self-heals. `false` (the default for a direct,
    /// reliable-transport consumer) keeps the emit-once model.
    pub loss_tolerant: bool,
    /// Aggregate-attach publication gate. While false, the actor retains the
    /// primed reference but emits no live state-sync frames.
    pub live_gate: watch::Receiver<bool>,
    /// Channel the actor uses to acknowledge the lifecycle insertion.
    /// `Ok(outcome)` on success (the outcome reports whether this actor
    /// is tick-managing the consumer); `Err(...)` if the per-consumer
    /// `SnapshotSynthesizer` or its priming pass could not be allocated.
    /// Dropping the receiver on the caller side is benign — the actor
    /// uses `send().ok()`.
    pub reply: oneshot::Sender<Result<ConsumerAttachOutcome, ConsumerAttachError>>,
}

/// Snapshot and exact actor cut produced atomically with state-sync
/// registration.
#[derive(Debug)]
pub struct StateSyncBootstrap {
    /// Synthesized VT snapshot captured from the canonical terminal.
    pub snapshot: SnapshotBytes,
    /// Actor-global raw sequence included by that same cut.
    pub base_seq: u64,
}

/// Successful outcome of a [`ConsumerAttachRequest`].
///
/// phux-3uv: the runtime needs to know whether this actor will *emit*
/// `TERMINAL_OUTPUT` for the consumer via the state-sync tick. If so, the
/// runtime must suppress its own broadcast pump. State-sync registration also
/// returns the snapshot captured in the same actor turn as its reference.
#[derive(Debug)]
pub struct ConsumerAttachOutcome {
    /// `true` when this actor's tick is the sole live emitter.
    pub tick_managed: bool,
    /// Atomic synthesized bootstrap for a state-sync consumer.
    pub state_sync_bootstrap: Option<StateSyncBootstrap>,
}

/// Errors surfaced by the private `TerminalActor::register_consumer`
/// path in response to a [`ConsumerAttachRequest`].
#[derive(Debug, thiserror::Error)]
pub enum ConsumerAttachError {
    /// libghostty refused to allocate the one-shot `RenderState` used to
    /// capture the consumer's initial cursor/mode state.
    #[error("libghostty allocation failed: {0}")]
    Ghostty(#[from] libghostty_vt::Error),
    /// Priming the per-consumer reference grid
    /// (`SnapshotSynthesizer::prime_reference`) failed.
    #[error("reference priming failed: {0}")]
    Synth(#[from] crate::grid::SynthesisError),
    /// The actor-global sequence cannot represent a post-bootstrap frame.
    #[error("state-sync sequence exhausted")]
    SequenceExhausted,
}

/// Request to drop the per-consumer state for `client_id`.
///
/// Sent by the runtime's DETACH path (and the EOF cleanup path). Silent
/// no-op if the consumer is not currently registered, matching
/// `ServerState::detach`'s idempotent contract.
#[derive(Debug)]
pub struct ConsumerDetachRequest {
    /// Identifier whose [`super::ConsumerSyncState`] entry to remove.
    pub client_id: ClientId,
    /// Fired once the entry has been removed (or was already absent).
    /// The caller can use this to sequence later operations against
    /// the actor; dropping the receiver is benign.
    pub reply: oneshot::Sender<()>,
}

/// Inbound `FRAME_ACK` request for the per-consumer state-sync loop
/// (phux-q0e.4 / ADR-0018 addendum).
///
/// Routed by `runtime.rs::handle_frame_ack` after it has resolved the
/// `wire_terminal_id` to a `TerminalActor`. The runtime is the only
/// thing that knows the `(client_id, wire_terminal_id) -> actor` mapping,
/// so it strips `terminal_id` before forwarding — the actor already
/// knows which terminal it is.
///
/// Silent no-op if `client_id` is not currently registered (matches the
/// idempotency of the rest of the consumer lifecycle). No reply: the
/// actor does the work in-process and the runtime fires-and-forgets.
#[derive(Debug)]
pub struct ConsumerAckRequest {
    /// Identifier whose [`super::ConsumerSyncState`]'s dirty cache to evict.
    pub client_id: ClientId,
    /// Logical subscription being acknowledged.
    pub stream_id: StreamId,
    /// Replica generation being acknowledged.
    pub bootstrap_id: BootstrapId,
    /// Cumulative ack sequence (per SPEC §12.2): the highest `seq` from
    /// `TERMINAL_OUTPUT` this consumer has applied. Strictly-monotonic
    /// against the per-consumer `last_acked_seq` — older/duplicate acks
    /// are silently dropped.
    pub seq: u64,
}

/// Default depth of the per-pane input mailbox.
///
/// Small on purpose: keystrokes are tiny and the server drains them in
/// the same event loop. A backed-up channel here would mean the actor
/// has stalled, which is its own bug to investigate.
pub const DEFAULT_INPUT_MAILBOX: usize = 64;

/// Bytes destined for the PTY writer, optionally carrying final write/flush
/// completion for acknowledged input.
#[derive(Debug)]
pub(crate) struct EncodedInputRequest {
    /// Fully encoded PTY bytes.
    pub(crate) bytes: Bytes,
    /// `true` after `write_all` and `flush` both succeed; `false` on either
    /// writer failure. Dropping the sender reports indeterminate delivery.
    pub(crate) completion: Option<std::sync::mpsc::Sender<bool>>,
}

impl EncodedInputRequest {
    pub(crate) fn legacy(bytes: Vec<u8>) -> Self {
        Self {
            bytes: bytes.into(),
            completion: None,
        }
    }

    pub(crate) fn opaque(bytes: Bytes) -> Self {
        Self {
            bytes,
            completion: None,
        }
    }

    pub(crate) fn acknowledged(bytes: Vec<u8>, completion: std::sync::mpsc::Sender<bool>) -> Self {
        Self {
            bytes: Bytes::from(bytes),
            completion: Some(completion),
        }
    }
}

/// Default capacity of the per-pane output broadcast channel.
///
/// Bytes fan out to subscribed clients. Sized for "burst tolerance" —
/// a busy pane can emit a few dozen frames in a short window before a
/// slow subscriber falls behind and gets a `RecvError::Lagged`.
pub const DEFAULT_OUTPUT_BROADCAST: usize = 256;

/// Continuity reason carried with an actor-generated full resync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResyncReason {
    /// Authoritative geometry changed.
    Resize,
    /// A bounded output subscriber observed a sequence gap.
    OutboundGap,
}
/// Payload of the per-pane output broadcast ([`TerminalHandle::output`]).
///
/// Subscribers (the per-attach output pumps in `runtime::attach`) map each
/// variant to a distinct wire frame:
///
/// * [`PaneOutput::Live`] → `TERMINAL_OUTPUT` — a post-snapshot byte delta.
/// * [`PaneOutput::Resync`] → `TERMINAL_SNAPSHOT` — the full post-reflow
///   grid, carrying the new `(cols, rows)` so the client mirror RESIZES to
///   them and repaints from authoritative state.
/// * [`PaneOutput::Control`] → an ordered generation/history control frame
///   routed only by the pump whose server-local owner matches.
///
/// Routing the resize-resync as a `TERMINAL_SNAPSHOT` (rather than raw
/// output) is load-bearing. The client resizes its libghostty mirror ONLY
/// on `TERMINAL_SNAPSHOT` (ADR-0013 / phux-wurs: the mirror's grid size is
/// server-authoritative and never guessed from a client-side rect). A
/// resync delivered as `TERMINAL_OUTPUT` would `vt_write` into a mirror
/// still at its old size, so a resize that GROWS a pane — kill-pane reflow
/// promoting the survivor, or enlarging the outer window — could never fill
/// the freed space (phux-3ns5). The snapshot path resizes first, then
/// applies the synthesized grid, so grow and shrink both reconverge.

#[derive(Clone, Debug)]
pub enum PaneOutput {
    /// Live PTY byte chunk forwarded as `TERMINAL_OUTPUT`.
    Live {
        /// Actor-global, strictly increasing raw output sequence.
        seq: u64,
        /// Verbatim PTY bytes for this sequence.
        bytes: Bytes,
    },
    /// Post-resize grid resync forwarded as `TERMINAL_SNAPSHOT` at the
    /// carried dims (phux-8v1 reconverge mechanism + phux-3ns5 mirror
    /// resize). `bytes` is the synthesized grid replay (with its
    /// `DECSTR + ED2 + home` reset preamble); `cols`/`rows` are the
    /// post-reflow grid size the client mirror must adopt.
    Resync {
        /// Post-reflow grid width the client mirror resizes to.
        cols: u16,
        /// Post-reflow grid height the client mirror resizes to.
        rows: u16,
        /// Why the prior generation can no longer continue.
        reason: ResyncReason,
        /// Actor-global raw sequence included by the replacement cut.
        base_seq: u64,
        /// Synthesized grid replay (with reset preamble) for `vt_write`.
        bytes: Bytes,
    },
    /// Ordered native control. It shares the broadcast sequence with
    /// [`Self::Live`] so a pump observes every prior raw sequence before
    /// invalidating the matching generation or history cursor.
    Control {
        /// Server-local pump owner; other subscribers ignore this control.
        owner: u64,
        /// Fully-owned tombstone/control frame for the matching pump.
        frame: FrameKind,
    },
}

/// Request for the pane's current `vt_replay_bytes` snapshot.
///
/// Sent by the ATTACH handler on the per-client task; the actor walks
/// its `Terminal` via [`crate::grid::SnapshotSynthesizer`] and replies on the
/// oneshot.
#[derive(Debug)]
pub struct SnapshotRequest {
    /// Requested scrollback history (`phux-9q5f`), carried from the
    /// `ATTACH.request_scrollback` / `scrollback_limit_lines` pair: `None`
    /// for viewport only, `Some(0)` for all retained history, `Some(n)` for
    /// the most-recent `n` rows. The actor primes
    /// [`SnapshotBytes::scrollback`] accordingly.
    pub scrollback: Option<u32>,
    /// Maximum aggregate snapshot bytes the actor may allocate.
    pub max_bytes: usize,
    /// Maximum frames the caller can still retain for this snapshot.
    pub max_frames: usize,
    /// Negotiated maximum bytes per synthesized bootstrap chunk.
    pub chunk_bytes: usize,
    /// Channel receiving the snapshot and actor-global raw cut.
    /// Dropping the receiver is benign; the actor discards the reply.
    pub reply:
        oneshot::Sender<Result<(SnapshotBytes, u64), crate::grid::SynthesisError>>,
}

/// Fully-owned native prefix captured atomically by the terminal actor.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativeBootstrapReply {
    /// Bounded BEGIN/CHUNK/READY sequence for the per-client pump to publish.
    pub frames: Vec<FrameKind>,
    /// Actor-global raw output cut included by the checkpoint.
    pub base_seq: u64,
}

/// Native checkpoint capture request serialized by the terminal actor.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Debug)]
pub struct NativeBootstrapRequest {
    /// Server-local owner identity used for detach and TTL cleanup.
    pub owner: u64,
    /// Wire terminal identity for this subscription.
    pub terminal_id: phux_protocol::ids::TerminalId,
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
    pub terminal_id: phux_protocol::ids::TerminalId,
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

/// Install the effective default palette reported by an interactive client.
///
/// The reply acknowledges that OSC 10/11 queries parsed after this point will
/// observe these values. Palette-less clients never send this request.
#[derive(Debug)]
pub struct SetDefaultColorsRequest {
    /// Outer terminal defaults to install on the canonical emulator.
    pub colors: phux_protocol::caps::TerminalDefaultColors,
    /// Completion acknowledgement.
    pub reply: oneshot::Sender<()>,
}

/// Request for the pane's current screen as structured data
/// (`phux-oki`, ADR-0022).
///
/// Sent by the `GET_SCREEN` command handler. The actor walks its own
/// `Terminal` into a [`phux_core::screen::ScreenState`] and replies on the
/// oneshot — side-effect-free: no resize, no client disturbance, unlike
/// the attach path.
#[derive(Debug)]
pub struct ScreenRequest {
    /// Wire-local pane id to stamp into the projected `ScreenState`.
    pub pane: u32,
    /// Requested scrollback history (`phux-o1v`): `None` for viewport only,
    /// `Some(0)` for all retained history, `Some(n)` for the most-recent
    /// `n` history rows. Carried from `GET_SCREEN.request_scrollback`.
    pub scrollback: Option<u32>,
    /// When `true`, populate [`phux_core::screen::ScreenState::cells`] with
    /// per-cell semantic marks + styles. Carried from `GET_SCREEN.cells`
    /// (`phux-8yl`).
    pub cells: bool,
    /// Channel the actor uses to ship the projection back. Dropping the
    /// receiver is benign — the actor discards the reply.
    pub reply: oneshot::Sender<phux_core::screen::ScreenState>,
}

/// Request for the pane's graceful-upgrade handoff (ADR-0032).
///
/// Sent by the upgrade producer while assembling the
/// [`StateBlob`](crate::upgrade::blob::StateBlob): it asks every pane's actor
/// for the descriptors and snapshot the re-exec'd image needs to re-adopt the
/// PTY and rebuild the `Terminal`. Side-effect-free — like [`SnapshotRequest`]
/// it only reads.
#[derive(Debug)]
pub struct UpgradeHandleRequest {
    /// Channel the actor uses to ship the [`PaneUpgradeHandle`] back. Dropping
    /// the receiver is benign — the actor discards the reply.
    pub reply: oneshot::Sender<PaneUpgradeHandle>,
}

/// One pane's contribution to the upgrade [`StateBlob`](crate::upgrade::blob::StateBlob):
/// the PTY descriptors to re-adopt plus the snapshot to replay.
///
/// `master_fd` / `child_pid` are `None` for a no-PTY actor (a pane carries no
/// child); the producer skips such panes since there is nothing to hand off.
#[derive(Debug, Clone)]
pub struct PaneUpgradeHandle {
    /// Raw PTY master descriptor (its `FD_CLOEXEC` is cleared by the
    /// orchestrator before the re-exec so it survives).
    pub master_fd: Option<RawFd>,
    /// Child PID on the slave side, re-adopted via `waitpid` after the exec.
    pub child_pid: Option<i32>,
    /// Current grid width in cells.
    pub cols: u16,
    /// Current grid height in cells.
    pub rows: u16,
    /// Per-cell pixel size, if any client reported pixel metrics (`None` when
    /// the actor's cell size is still the unset `0x0`).
    pub cell_px: Option<(u16, u16)>,
    /// Current pane title, if the child set one.
    pub title: Option<String>,
    /// Live working directory (kernel query against the child), falling back
    /// to the actor's last-known CWD.
    pub cwd: Option<String>,
    /// Replayable viewport snapshot — the same bytes a freshly-attaching
    /// client receives.
    pub vt_replay_bytes: Vec<u8>,
    /// Replayable scrollback that precedes the viewport, or empty.
    pub scrollback_bytes: Vec<u8>,
}

/// Request for the pane's live current working directory (`phux-cs6`).
///
/// Sent by the `SPAWN_TERMINAL` handler when `defaults.cwd-inheritance`
/// is [`phux_config::CwdInheritance::InheritFocused`] and the wire frame
/// left `cwd` unset: the new pane should open in the focused pane's live
/// CWD. The actor asks the kernel for its PTY child's working directory
/// via [`crate::cwd_query::process_cwd`] (the shell's directory *now*,
/// after any `cd`) and replies on the oneshot. Side-effect-free: no
/// resize, no client disturbance.
///
/// This deliberately does not use libghostty's `Terminal::pwd`: the
/// bundled libghostty surfaces OSC 7 only as an opaque `ReportPwd`
/// command without exposing the announced path, so that getter never
/// populates from the byte stream. The kernel query also needs no shell
/// OSC 7 configuration.
///
/// The reply is `None` when there is no PTY (no-PTY actor), the child has
/// no pid (already exited), or the platform query is unsupported/denied —
/// the caller then falls back to a non-inherited default.
#[derive(Debug)]
pub struct PwdRequest {
    /// Channel the actor uses to ship the working directory back.
    /// `None` ⇒ no resolvable CWD. Dropping the receiver is benign — the
    /// actor discards the reply.
    pub reply: oneshot::Sender<Option<String>>,
}

/// A resize request delivered to a [`super::TerminalActor`] over its `resize`
/// mailbox.
///
/// `resync_clients` controls the phux-8v1 post-resize behavior: when
/// `true`, the actor re-broadcasts a full grid snapshot after the reflow
/// so attached clients (whose mirror reflowed independently and may have
/// dropped rows) reconverge. It is `true` for *live* resizes from an
/// already-attached client (SIGWINCH → `VIEWPORT_RESIZE`/`TERMINAL_RESIZE`)
/// and `false` for the ATTACH-time resize — the attach handshake already
/// sends an authoritative `TERMINAL_SNAPSHOT`, and a resync broadcast
/// there would race ahead of it and reorder the handshake.
#[derive(Debug, Clone, Copy)]
pub struct ResizeRequest {
    /// New grid width in cells.
    pub cols: u16,
    /// New grid height in cells.
    pub rows: u16,
    /// Pixel size of one cell `(width, height)` on the donor display, as
    /// resolved by [`crate::state::ServerState::resolve_terminal_cell_px`].
    /// The actor multiplies it out to the PTY `winsize` pixel fields and
    /// libghostty's pixel dimensions (XTWINOPS size replies, image
    /// protocols). `None` ⇒ no pixel truth in this request; the actor
    /// keeps its last-known cell size so a pixel-less resize (e.g. an
    /// agent's `TERMINAL_RESIZE`) cannot zero out geometry a real client
    /// already established.
    pub cell_px: Option<(u16, u16)>,
    /// Re-broadcast a full snapshot after reflow (live resize) vs stay
    /// quiet (attach-time resize). See the type-level doc.
    pub resync_clients: bool,
    /// Resync without resizing: skip the `handle_resize` entirely and only
    /// schedule the `resync_clients` broadcast. Used by a lagged output pump to
    /// ask the actor to emit an in-band [`PaneOutput::Resync`] (a full grid
    /// snapshot on the same ordered broadcast) so a consumer that dropped bytes
    /// past the broadcast buffer reconverges — without disturbing the grid
    /// geometry. `cols`/`rows`/`cell_px` are ignored when this is set.
    pub resync_only: bool,
}

/// Cross-task handle to a [`super::TerminalActor`].
///
/// `TerminalHandle` is `Send + Clone`: per-client tasks clone it freely to
/// request snapshots, send input, or subscribe to the output broadcast.
/// The actor itself (which owns the `!Send` `Terminal`) lives on the
/// `LocalSet` and never crosses a thread boundary.
#[derive(Debug, Clone)]
pub struct TerminalHandle {
    /// Sender for input events (keys, mouse, etc.). Drained by the
    /// actor and written to the PTY via the per-pane encoders.
    pub input: mpsc::Sender<TerminalInput>,
    /// Bounded handoff for bytes already encoded on the dedicated input lane.
    /// The lane uses `try_send`, so a saturated actor never blocks input
    /// routing or grows memory without bound.
    pub(crate) encoded_input: mpsc::Sender<EncodedInputRequest>,
    /// Latest complete `Send` input-encoder state captured by the actor after
    /// every terminal mutation.
    pub input_snapshot: tokio::sync::watch::Receiver<crate::input::InputEncoderSnapshot>,
    /// Sender for snapshot requests. The ATTACH handler uses this to
    /// build `TERMINAL_SNAPSHOT` frames.
    pub snapshot: mpsc::Sender<SnapshotRequest>,
    /// Actor-serialized native checkpoint capture.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub native_bootstrap: mpsc::Sender<NativeBootstrapRequest>,
    /// Actor-serialized native history paging.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub native_history: mpsc::Sender<NativeHistoryRequest>,
    /// Actor-serialized detach cleanup for retained native cuts.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub native_release: mpsc::Sender<NativeReleaseRequest>,
    /// Sender for host-palette updates. The most recently attached client
    /// that advertises colors is authoritative for the shared pane.
    pub set_default_colors: mpsc::Sender<SetDefaultColorsRequest>,
    /// Sender for structured screen reads. The `GET_SCREEN` command
    /// handler uses this to project the pane's grid to JSON without
    /// attaching (`phux-oki`, ADR-0022 §5).
    pub screen: mpsc::Sender<ScreenRequest>,
    /// Sender for working-directory reads (`phux-cs6`). The
    /// `SPAWN_TERMINAL` handler uses this to resolve
    /// `defaults.cwd-inheritance = inherit-focused`: it asks the focused
    /// pane's actor for its live CWD (a kernel query against the PTY
    /// child, see [`PwdRequest`]) and seeds the new pane's
    /// `CommandBuilder.cwd` with it.
    pub pwd: mpsc::Sender<PwdRequest>,
    /// Output broadcast channel; subscribers receive every PTY byte
    /// chunk ([`PaneOutput::Live`]) plus post-resize grid resyncs
    /// ([`PaneOutput::Resync`]) forwarded by the actor.
    pub output: broadcast::Sender<PaneOutput>,
    /// Resize control channel. The actor honours each request by
    /// resizing libghostty's `Terminal` and the PTY winsize ioctl, and
    /// (when [`ResizeRequest::resync_clients`] is set) re-broadcasting a
    /// full grid snapshot so client mirrors reconverge after reflow
    /// (phux-8v1).
    pub resize: mpsc::Sender<ResizeRequest>,
    /// ADR-0018 per-consumer state-sync lifecycle (phux-q0e.2). The
    /// runtime sends a [`ConsumerAttachRequest`] on each successful
    /// ATTACH so the actor allocates the per-consumer `RenderState`
    /// before the `TERMINAL_SNAPSHOT` goes out. Future state-sync work
    /// (phux-q0e.3 tick driver, phux-q0e.4 `FRAME_ACK`) reads from the
    /// resulting per-consumer state map.
    pub consumer_attach: mpsc::Sender<ConsumerAttachRequest>,
    /// Counterpart to [`Self::consumer_attach`]. The runtime sends
    /// this on DETACH (and on the EOF cleanup path) to free the
    /// per-consumer `RenderState`. Silent no-op if the consumer was
    /// never attached.
    pub consumer_detach: mpsc::Sender<ConsumerDetachRequest>,
    /// ADR-0018 inbound `FRAME_ACK` channel (phux-q0e.4). The runtime
    /// sends one [`ConsumerAckRequest`] per decoded `FRAME_ACK` whose
    /// `terminal_id` resolved to this actor; the actor evicts the
    /// per-consumer dirty cache so the next tick re-diffs against the
    /// freshly-acked reference. Silent no-op if the consumer is not
    /// currently registered.
    pub consumer_ack: mpsc::Sender<ConsumerAckRequest>,
    /// Subscribe to semantic terminal events for this pane. The runtime sends
    /// a [`SubscribeToEventsRequest`] when a client subscribes; the actor
    /// registers the subscriber and begins broadcasting matching events.
    pub subscribe_to_events: mpsc::Sender<SubscribeToEventsRequest>,
    /// Unsubscribe from semantic terminal events. The runtime sends an
    /// [`UnsubscribeFromEventsRequest`] when a client detaches; the actor
    /// removes the subscriber from its list (idempotent).
    pub unsubscribe_from_events: mpsc::Sender<UnsubscribeFromEventsRequest>,
    /// Graceful-upgrade handoff channel (ADR-0032). The upgrade producer sends
    /// an [`UpgradeHandleRequest`] per pane to collect its PTY descriptors +
    /// replay snapshot into the [`StateBlob`](crate::upgrade::blob::StateBlob).
    pub upgrade: mpsc::Sender<UpgradeHandleRequest>,
    /// Supervisory control channel (ADR-0033). The runtime sends a
    /// [`ControlRequest`] when a client takes the wheel, releases it, or
    /// signals the pane's process group. The actor delivers signals (it owns
    /// the PTY child pid) and broadcasts `TerminalControl` events (it owns the
    /// event-subscriber list).
    pub control: mpsc::Sender<ControlRequest>,
    /// Pane viewport width in cells at construction time.
    pub cols: u16,
    /// Pane viewport height in cells at construction time.
    pub rows: u16,
}

/// A client subscribed to semantic events for a single Terminal (pane).
/// Holds the client's outbound mailbox and event type filter.
#[derive(Clone, Debug)]
pub struct TerminalEventSubscriber {
    /// Client's outbound frame channel (where Event frames are sent).
    pub outbound: tokio::sync::mpsc::Sender<Outbound>,
    /// Event type filter (empty = all types). Only events matching a type
    /// in this list are forwarded; if empty, all events are sent.
    pub event_types: Vec<TerminalEventType>,
}

/// Request to subscribe to semantic terminal events.
#[derive(Debug)]
pub struct SubscribeToEventsRequest {
    /// The new subscriber to register.
    pub subscriber: TerminalEventSubscriber,
    /// Wire-level terminal id for Event frames (SPEC §7.1).
    /// The runtime passes this when registering.
    pub wire_terminal_id: u32,
}

/// Request to unsubscribe from semantic terminal events.
#[derive(Debug)]
pub struct UnsubscribeFromEventsRequest {
    /// Address of the subscriber's outbound mailbox, used for identity
    /// comparison against the actor's registered subscribers.
    ///
    /// A `usize` address rather than a `*const Sender<Outbound>`: a raw
    /// pointer is `!Send`, which would make [`TerminalHandle`] — and through
    /// it the entire [`ServerState`](crate::state::ServerState) — `!Send`,
    /// blocking the dedicated input lane (phux-51n6.2, ADR-0044) that routes
    /// input from a separate thread. The identity semantics are unchanged: the
    /// actor compares this against `&raw const sub.outbound as usize`, exactly
    /// as the prior pointer equality did.
    pub outbound_addr: usize,
}
