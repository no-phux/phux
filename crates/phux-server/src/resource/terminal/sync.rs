//! Submodule for terminal actor internals.

use super::tick::RttEstimator;
use crate::grid::ConsumerReference;
use crate::mailbox::Outbound;
use libghostty_vt::{
    Terminal as GhosttyTerminal,
    render::{CursorVisualStyle, Snapshot},
    terminal::Mode,
};
use phux_protocol::ids::{BootstrapId, StreamId};
use tokio::sync::{mpsc, watch};

/// Cursor and DEC mode bits captured when a consumer is brought up to date,
/// compared by the tick to decide whether to re-emit the epilogue.
///
/// Tracks the modes the snapshot epilogue re-emits; keep them in step.
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "DEC mode bits are independent flags; collapsing them into a bitfield obscures the per-flag mapping to `Mode::*` constants"
)]
pub struct LastAckedCursorMode {
    /// Cursor column; `None` when the cursor is off the viewport.
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
    /// DEC 1049. Tracked with 47 because they are independent bits and a
    /// 47<->1049 switch must still diff.
    pub alt_screen_save: bool,
}

impl LastAckedCursorMode {
    /// Capture the live cursor and modes; FFI errors degrade to safe
    /// defaults (cursor hidden, modes off).
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

    /// Placeholder for a raw consumer, whose capture is never read.
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

/// Cap on [`ConsumerSyncState::emit_instants`] (and pending refs), for a
/// consumer that never acks: about 5 s of ticks at the 20 ms floor.
pub const MAX_EMIT_INSTANTS: usize = 256;

/// Per-consumer state-sync cache (ADR-0018), one per attached consumer on
/// the actor. `!Send` like the terminal it tracks.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent per-consumer state flags (needs_initial_emit, behind, \
              wants_state_sync, loss_tolerant); collapsing them into an enum would \
              obscure the per-flag lifecycle each drives"
)]
pub struct ConsumerSyncState {
    /// Last-synced rendered row bodies and cursor/mode state. The tick diffs
    /// the live grid against it and advances it on emit, independent of
    /// libghostty's shared dirty bits.
    pub reference: ConsumerReference,
    /// Outbound mailbox for tick-emitted frames.
    pub outbound: mpsc::Sender<Outbound>,
    /// Wire terminal id for the `ResourceOutput` frame.
    pub wire_terminal_id: u32,
    /// Logical protocol-0.7 subscription identity.
    pub stream_id: StreamId,
    /// Replica generation currently receiving live output.
    pub bootstrap_id: BootstrapId,
    /// Aggregate-attach gate; false suppresses live output until `ATTACH_READY`.
    pub live_gate: watch::Receiver<bool>,
    /// Per-consumer `RESOURCE_OUTPUT` sequence, starting at 1.
    pub next_seq: u64,
    /// Highest acked `seq`; `0` before any ack.
    pub last_acked_seq: u64,
    /// Cursor/mode bits at the last sync point.
    pub last_cursor_mode: LastAckedCursorMode,
    /// Set at registration: walk once on the next tick even if the terminal
    /// is clean.
    pub needs_initial_emit: bool,
    /// A tick skipped this consumer on a full mailbox, so its reference is
    /// behind the grid; keeps the walk going on a clean terminal until it
    /// is served.
    pub behind: bool,
    /// Smoothed RTT for the adaptive cadence.
    pub rtt: RttEstimator,
    /// Emit instants of in-flight `seq`s, for RTT on ack. Pruned on ack and
    /// capped at [`MAX_EMIT_INSTANTS`].
    pub emit_instants: std::collections::BTreeMap<u64, tokio::time::Instant>,
    /// Whether this consumer negotiated `StateSync`; otherwise the broadcast
    /// pump serves it (unless the test override forces the tick).
    pub wants_state_sync: bool,
    /// Loss-tolerant model (ADR-0042): diff against [`Self::acked_reference`],
    /// advance only on ack, and retransmit after a timeout, so drops on a
    /// forwarded leg self-heal. Off by default (emit-once).
    pub loss_tolerant: bool,
    /// The last-acked reference (loss-tolerant only).
    pub acked_reference: ConsumerReference,
    /// Snapshots of emitted, un-acked frames by `seq`; an ack advances
    /// [`Self::acked_reference`] to the highest covered one. Capped at
    /// [`MAX_EMIT_INSTANTS`].
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
