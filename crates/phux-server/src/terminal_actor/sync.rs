//! Submodule for terminal actor internals.

use super::tick::RttEstimator;
use crate::grid::ConsumerReference;
use crate::state::Outbound;
use libghostty_vt::{
    Terminal as GhosttyTerminal,
    render::{CursorVisualStyle, Snapshot},
    terminal::Mode,
};
use phux_protocol::ids::{BootstrapId, StreamId};
use tokio::sync::{mpsc, watch};

/// Snapshot of the live `Terminal`'s cursor + DEC mode bits captured at
/// the moment a consumer is brought up-to-date.
///
/// Captured via ATTACH today; via `FRAME_ACK` in phux-q0e.4. The
/// state-sync tick driver (phux-q0e.3) compares this against the live
/// terminal's current state to decide whether the per-tick incremental
/// synthesis must re-emit the cursor placement + DEC modes that
/// `SnapshotSynthesizer::synthesize` would emit at the tail of a
/// from-empty snapshot.
///
/// The set tracked here mirrors the modes that today's
/// `SnapshotSynthesizer::synthesize` re-emits at the end of a snapshot
/// (`BRACKETED_PASTE`, `FOCUS_EVENT`, `ALT_SCREEN_LEGACY`) plus the
/// cursor placement/visibility/style read off the `RenderState::Snapshot`.
/// New mode bits that synthesize starts re-emitting should be added here
/// in lock-step.
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "DEC mode bits are independent flags; collapsing them into a bitfield obscures the per-flag mapping to `Mode::*` constants"
)]
pub struct LastAckedCursorMode {
    /// Cursor column (zero-based viewport coords). `None` when the
    /// cursor is not viewport-resident (off-screen due to scrollback).
    pub cursor_x: Option<u16>,
    /// Cursor row (zero-based viewport coords).
    pub cursor_y: Option<u16>,
    /// `DECTCEM` (DEC private mode 25): cursor visibility.
    pub cursor_visible: bool,
    /// `DECSCUSR` shape.
    pub cursor_visual_style: CursorVisualStyle,
    /// `DECSCUSR` blink flag.
    pub cursor_blinking: bool,
    /// `BRACKETED_PASTE` (DEC private mode 2004).
    pub bracketed_paste: bool,
    /// `FOCUS_EVENT` (DEC private mode 1004).
    pub focus_event: bool,
    /// `ALT_SCREEN_LEGACY` (DEC private mode 47).
    pub alt_screen_legacy: bool,
    /// `ALT_SCREEN` (DEC private mode 1047).
    pub alt_screen: bool,
    /// `ALT_SCREEN_SAVE` (DEC private mode 1049) — the mode vim/less/man/
    /// htop/tmux actually use. Tracked alongside 47 so a 47<->1049
    /// transition still trips the diff trigger; 47 and 1049 are
    /// independent bits in libghostty, so tracking only 47 would miss it.
    pub alt_screen_save: bool,
}

impl LastAckedCursorMode {
    /// Capture the live terminal's cursor + DEC mode state into a fresh
    /// `LastAckedCursorMode`. Querying every field; libghostty FFI errors
    /// degrade to safe defaults (cursor invisible, modes off) so a
    /// transient FFI failure doesn't kill the actor.
    pub(crate) fn capture(terminal: &GhosttyTerminal<'_, '_>, snapshot: &Snapshot<'_, '_>) -> Self {
        let (cursor_x, cursor_y) = match snapshot.cursor_viewport() {
            Ok(Some(v)) => (Some(v.x), Some(v.y)),
            Ok(None) | Err(_) => (None, None),
        };
        Self {
            cursor_x,
            cursor_y,
            cursor_visible: snapshot.cursor_visible().unwrap_or(false),
            cursor_visual_style: snapshot
                .cursor_visual_style()
                .unwrap_or(CursorVisualStyle::Block),
            cursor_blinking: snapshot.cursor_blinking().unwrap_or(false),
            bracketed_paste: terminal.mode(Mode::BRACKETED_PASTE).unwrap_or(false),
            focus_event: terminal.mode(Mode::FOCUS_EVENT).unwrap_or(false),
            alt_screen_legacy: terminal.mode(Mode::ALT_SCREEN_LEGACY).unwrap_or(false),
            alt_screen: terminal.mode(Mode::ALT_SCREEN).unwrap_or(false),
            alt_screen_save: terminal.mode(Mode::ALT_SCREEN_SAVE).unwrap_or(false),
        }
    }

    /// A placeholder capture for a raw broadcast-pump consumer, whose
    /// per-consumer cursor/mode state is never read (the tick path serves
    /// only tick-managed consumers and `FRAME_ACK` is dropped for raw ones).
    /// Uses the same safe defaults `capture` falls back to on FFI error, so
    /// it costs no terminal walk on the human attach path. If such a consumer
    /// were ever served by the tick, `needs_initial_emit` forces a full pass
    /// that overwrites this.
    pub(crate) const fn unprimed() -> Self {
        Self {
            cursor_x: None,
            cursor_y: None,
            cursor_visible: false,
            cursor_visual_style: CursorVisualStyle::Block,
            cursor_blinking: false,
            bracketed_paste: false,
            focus_event: false,
            alt_screen_legacy: false,
            alt_screen: false,
            alt_screen_save: false,
        }
    }
}

/// Defensive cap on [`ConsumerSyncState::emit_instants`].
///
/// The map is pruned to the in-flight window on every `FRAME_ACK`, so a
/// well-behaved consumer keeps only a handful of entries. This cap guards the
/// pathological case — a consumer that opts into state sync but never acks, or
/// a transport that drops acks — where the map would otherwise grow one entry
/// per emitted tick without bound. 256 entries is ~5s of in-flight ticks at
/// the [`MIN_TICK_INTERVAL`](super::tick::MIN_TICK_INTERVAL) floor (20ms),
/// far beyond any real RTT window, and bounds the map to a few KB per consumer.
pub const MAX_EMIT_INSTANTS: usize = 256;

/// Per-consumer cached reference state for ADR-0018 lazy state
/// synchronization. One per `(TerminalActor, attached ClientId)`.
///
/// Holds the libghostty `RenderState` that tracks "what cells this
/// consumer has already seen" (so the tick driver in phux-q0e.3 can
/// walk only the rows that changed since the last per-consumer
/// snapshot), the `seq` of the last frame this consumer `ACK`ed (driven
/// by phux-q0e.4's `FRAME_ACK` handler), and the cursor/mode state
/// captured at the same instant.
///
/// `Drop` runs the libghostty `ghostty_render_state_free` via
/// `RenderState`'s own destructor — no explicit cleanup needed in
/// DETACH beyond removing the entry from the actor's map.
///
/// `!Send + !Sync` because `RenderState` is `!Send + !Sync` (per the
/// `libghostty-send-sync` bd memory). Lives only inside the actor,
/// which runs on the `LocalSet` thread that owns the `Terminal`.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent per-consumer state flags (needs_initial_emit, behind, \
              wants_state_sync, loss_tolerant); collapsing them into an enum would \
              obscure the per-flag lifecycle each drives"
)]
pub struct ConsumerSyncState {
    /// Per-consumer reference grid for the lazy state-sync diff
    /// (phux-ia4). Holds the last-synced rendered body of every viewport
    /// row plus the last-synced cursor/mode state. The tick driver diffs
    /// the live `Terminal` against this (via the actor's shared
    /// [`crate::grid::SnapshotSynthesizer`]) and advances it on emit.
    ///
    /// This replaces the earlier per-consumer `RenderState` dirty cache.
    /// `RenderState::update` *consumes* the shared `Terminal` dirty bits
    /// on the first read each tick (libghostty `render.zig`), so a
    /// per-consumer `RenderState` could not isolate dirty across N
    /// consumers on one pane: the first consumer's `update` starved the
    /// rest. The reference grid is fully independent per consumer and
    /// never reads the shared dirty bits, so every consumer gets its own
    /// correct diff each tick regardless of attach/ack divergence. See
    /// [`crate::grid::SnapshotSynthesizer::synthesize_against_reference`].
    pub reference: ConsumerReference,
    /// Per-consumer outbound mailbox the tick driver pushes
    /// `TERMINAL_OUTPUT` frames into. Cloned from the
    /// [`crate::state::AttachedClient`]'s `tx` at ATTACH time.
    pub outbound: mpsc::Sender<Outbound>,
    /// Wire-level terminal id for the `TerminalOutput` frame
    /// (`docs/spec/L1.md` §2.1). Carried per-consumer because the runtime owns
    /// the mapping `(TerminalActor, WireTerminalId)` and may differ
    /// across consumers in future tier topologies.
    pub wire_terminal_id: u32,
    /// Logical protocol-0.7 subscription identity.
    pub stream_id: StreamId,
    /// Replica generation currently receiving live output.
    pub bootstrap_id: BootstrapId,
    /// Aggregate-attach gate; false suppresses live output until ATTACH_READY.
    pub live_gate: watch::Receiver<bool>,
    /// Per-consumer monotonic sequence id for `TERMINAL_OUTPUT`
    /// (`docs/spec/L1.md` §2.1, §12). Starts at `1` and increments on each
    /// emitted frame. Per-consumer (not shared) so each consumer can
    /// `FRAME_ACK` against its own stream — this matches the existing
    /// per-pump scheme in `runtime.rs::handle_attach`.
    pub next_seq: u64,
    /// `FrameId` of the most recent `TERMINAL_OUTPUT` this consumer has
    /// `ACK`ed. `0` means "no acks yet — the next emission is the only
    /// thing this consumer has seen" (matches `FrameId::ZERO`'s "empty
    /// initial frame" semantics).
    pub last_acked_seq: u64,
    /// Cursor + DEC mode bits captured at the last sync point. Used by
    /// the tick driver to decide whether to re-emit cursor placement /
    /// mode toggles in the incremental synthesis path. See
    /// [`LastAckedCursorMode`] for the field set rationale.
    pub last_cursor_mode: LastAckedCursorMode,
    /// Set `true` at registration, cleared after this consumer's first
    /// pass through the actor's per-tick synthesis (phux-4l0).
    ///
    /// The idle short-circuit skips the per-consumer row walk when the
    /// shared terminal is `Clean` since the previous tick. A consumer
    /// registered *after* the last write sits on a `Clean` terminal yet
    /// has never been diffed; its reference is primed (so the diff is
    /// empty and emit-once still holds), but we must still run its first
    /// synthesis pass rather than silently skip it — this is the
    /// "needs prime / has diverged" case the phux-ia4 fix must preserve.
    /// While any consumer has this set, the short-circuit is suppressed.
    pub needs_initial_emit: bool,
    /// Set when a tick skipped this consumer because its outbound mailbox
    /// was full (backpressure), so its reference is *behind* the live grid
    /// even though the grid has not mutated since. The idle short-circuit
    /// must not skip the per-consumer loop while any consumer is behind, or
    /// the held-back delta would never be retried once the client drains
    /// (the terminal can stay `Clean` indefinitely). Cleared the moment this
    /// consumer is successfully served — either a delta ships or the diff is
    /// empty (reference caught up to the grid).
    pub behind: bool,
    /// Smoothed RTT estimate for the RTT-adaptive tick cadence (phux-q0e.5).
    /// Fed one sample per `FRAME_ACK`; drives this consumer's desired tick
    /// interval. See [`RttEstimator`].
    pub rtt: RttEstimator,
    /// Emit timestamps for in-flight (emitted, not-yet-acked) `seq`s, used to
    /// measure RTT server-side when the matching `FRAME_ACK` arrives
    /// (phux-q0e.5). Keyed by the `seq` stamped on each `TERMINAL_OUTPUT`;
    /// the value is the `tokio::time::Instant` the frame was handed to the
    /// outbound mailbox. Pruned up to the acked `seq` on every ack so it
    /// stays bounded by the number of frames in flight within one RTT (a
    /// handful at the clamped cadence) for an acking consumer, and hard-capped
    /// at [`MAX_EMIT_INSTANTS`] (oldest-evicted) so a never-acking consumer
    /// cannot grow it without bound. No wire change: the RTT round-trip
    /// rides the `seq` that `FRAME_ACK` already echoes.
    pub emit_instants: std::collections::BTreeMap<u64, tokio::time::Instant>,
    /// Whether this consumer negotiated the synthesized state-sync tick
    /// emitter (phux-fseo). When `true`, `tick_emit` serves this consumer
    /// even with the global test gate off; when `false` the consumer is
    /// served by the runtime's raw broadcast pump and `tick_emit` stays
    /// silent for it. The global `consumer_tick_emits` test override still
    /// forces emission for every consumer regardless of this flag.
    pub wants_state_sync: bool,
    /// Whether this consumer is on a lossy/forwarded leg and uses the
    /// advance-on-ack loss-tolerant emission model (phux-v45.8, ADR-0042).
    ///
    /// `false` (the default) keeps the v0.1 emit-once model: the reference
    /// (`reference`) advances on emit and `FRAME_ACK` only drives backpressure
    /// accounting — correct and cheapest on a reliable ordered transport (UDS,
    /// SSH stdio, WebSocket, a QUIC reliable stream). `true` switches to the
    /// Mosh loss-tolerant model: each tick re-diffs the live grid against
    /// [`Self::acked_reference`] (the last state the consumer provably has), the
    /// reference advances only when a matching `FRAME_ACK` lands, and an
    /// un-acked frame is retransmitted after a retransmit timeout — so a frame
    /// the forwarded leg dropped self-heals rather than diverging the mirror
    /// forever. Opt-in per consumer; the reliable-transport default path is
    /// untouched.
    pub loss_tolerant: bool,
    /// The last-*acked* reference grid — the diff base for the loss-tolerant
    /// model (phux-v45.8). Unused (empty) unless [`Self::loss_tolerant`]. Primed
    /// to the live grid when loss-tolerance is enabled (the consumer's
    /// `TERMINAL_SNAPSHOT` brings its mirror to this same point); advanced by
    /// the actor's `on_frame_ack` to the grid snapshot a cumulative ack covers.
    pub acked_reference: ConsumerReference,
    /// Grid snapshots of each emitted-but-not-yet-acked frame, keyed by the
    /// `seq` stamped on its `TERMINAL_OUTPUT` (phux-v45.8). On `FRAME_ACK` the
    /// consumer's [`Self::acked_reference`] advances to the snapshot of the
    /// highest `seq` the (cumulative) ack covers, and every entry at or below it
    /// is pruned. Bounded by the in-flight window for an acking consumer and
    /// hard-capped at [`MAX_EMIT_INSTANTS`] (oldest-evicted) so a wedged leg
    /// cannot grow it without bound. Empty unless [`Self::loss_tolerant`].
    pub pending_refs: std::collections::BTreeMap<u64, ConsumerReference>,
}

impl std::fmt::Debug for ConsumerSyncState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsumerSyncState")
            .field("wire_terminal_id", &self.wire_terminal_id)
            .field("next_seq", &self.next_seq)
            .field("last_acked_seq", &self.last_acked_seq)
            .field("last_cursor_mode", &self.last_cursor_mode)
            .field("wants_state_sync", &self.wants_state_sync)
            .finish_non_exhaustive()
    }
}
