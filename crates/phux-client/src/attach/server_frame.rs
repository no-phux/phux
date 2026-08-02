//! Server-to-client frame handling: dispatches `FrameKind` variants to
//! the right state mutations and rendering.
//!
//! Returns a `FrameOutcome` describing the follow-up the async driver
//! should take (e.g. exit on `DETACHED`, send `GET_METADATA` after
//! `ATTACHED`, repaint after a layout-replacing frame).

use std::collections::{HashMap, HashSet};

use phux_client_core::engine::CanonicalGeometry;
use phux_client_core::session::{
    EffectBuffer as KernelEffectBuffer, HistoryRejectionReason as KernelHistoryRejectionReason,
    HistoryUnavailableReason, KernelEffect, KernelInput, KernelSend,
};
use phux_protocol::ids::{ClientId, SessionId, TerminalId};
use phux_protocol::wire::frame::{
    AgentEvent, CONFIG_RELOAD_KEY, ErrorCode, FrameKind, HistoryRejectionReason,
    HistoryTombstoneReason, Scope, SpawnError, SpawnResult,
};
use phux_protocol::wire::info::SessionInfo;
use phux_protocol::{BootstrapId, StreamId};

use super::actions::{self, PendingSplit, PendingWindow, apply_spawned_ok, apply_terminal_closed};
use super::driver::{AttachEnd, AttachError, DEFAULT_GROUP_ID, PaneSlot};
use super::paint::{SidebarReservation, content_rect, paint_bar_after_pane, paint_focused_pane};
use crate::agent_meta::{AgentRecord, TERMINAL_AGENT_KEY, parse_agent_record};
use crate::layout::{self, LayoutState, Workspace};
use crate::predict::{Overlay, PredictionState, reconcile_terminal_output_per_cell};
use crate::render::chrome::status_bar::{Notice, StatusBarPainter};

/// ADR-0040 (`phux-3ert`): the driver-held index of `phux.agent/v1` records.
///
/// `records` is what the window chrome reads (structured agent labels for
/// the sidebar/tab strip); `pending` correlates in-flight `GET_METADATA`
/// request ids to the Terminal they asked about; `subscribed` tracks which
/// Terminals already have a live `SUBSCRIBE_METADATA` so the driver's
/// subscription sweep is idempotent.
#[derive(Debug, Default)]
pub(super) struct AgentMetaIndex {
    /// Terminal → its decoded agent record (absent = no declared agent).
    pub(super) records: HashMap<TerminalId, AgentRecord>,
    /// In-flight `GET_METADATA` request id → the Terminal it targets.
    pub(super) pending: HashMap<u32, TerminalId>,
    /// Terminals with a live `SUBSCRIBE_METADATA` on the agent key.
    pub(super) subscribed: std::collections::HashSet<TerminalId>,
    /// Terminal → when its record last actually changed. The attention
    /// ladder's tiebreak: rows of equal rank sort most-recently-changed
    /// first, so the agent that just flipped to `blocked` sits above one that
    /// has been blocked for an hour.
    ///
    /// Lives HERE, driver-side, and never inside
    /// [`crate::render::chrome::sidebar::AgentEntry`]: that struct is the
    /// sidebar painter's content-cache key, and a timestamp in it would miss
    /// the cache on every frame and repaint the strip forever. This map
    /// influences only the row ORDER.
    pub(super) change_at: HashMap<TerminalId, std::time::Instant>,
}

impl AgentMetaIndex {
    /// Apply a metadata value for `terminal` (a `GET` reply or a
    /// `METADATA_CHANGED` broadcast; `None` bytes = tombstone). Returns
    /// `true` when the stored record actually changed, so the driver only
    /// repaints chrome for real transitions.
    ///
    /// A real change also stamps [`Self::change_at`]; a tombstone clears it,
    /// so a retracted record (the agent exited) leaves nothing behind to sort
    /// by.
    fn apply(&mut self, terminal: &TerminalId, bytes: Option<&[u8]>) -> bool {
        let changed = match bytes.and_then(parse_agent_record) {
            Some(record) => self.records.insert(terminal.clone(), record.clone()) != Some(record),
            None => self.records.remove(terminal).is_some(),
        };
        if changed {
            if self.records.contains_key(terminal) {
                self.change_at
                    .insert(terminal.clone(), std::time::Instant::now());
            } else {
                self.change_at.remove(terminal);
            }
        }
        changed
    }
}

/// Fold a real agent-record change into the attention ladder's per-pane
/// bookkeeping.
///
/// A NEW state on a pane the user is not currently looking at is UNSEEN — even
/// if they visited that pane an hour ago. That is precisely the signal the
/// sidebar's "finished but unreviewed" tier is built on: the pane went `done`
/// while the user's attention was elsewhere, so it must climb above the agents
/// that are merely still working. A change on the FOCUSED pane is seen by
/// definition — the user is watching it happen — so it never re-arms.
fn note_agent_change(
    panes: &mut HashMap<TerminalId, PaneSlot>,
    focused_pane: Option<&TerminalId>,
    terminal: &TerminalId,
) {
    if focused_pane == Some(terminal) {
        return;
    }
    if let Some(slot) = panes.get_mut(terminal) {
        slot.seen = false;
    }
}

/// Outcome of processing a single server-to-client frame.
///
/// The driver translates these into async actions (send a frame, exit
/// the loop, repaint). Keeping the side-effect-free decision inside
/// [`handle_server_frame`] lets the function stay synchronous.
#[allow(
    clippy::struct_excessive_bools,
    reason = "parallel server-frame outcome flags; refactor into bitset would obscure callers"
)]
#[derive(Debug, Clone, Default)]
pub(super) struct FrameOutcome {
    /// `true` ⇒ the loop should exit cleanly: either the server sent
    /// `DETACHED`, or a `TERMINAL_CLOSED` folded the last pane out of the
    /// layout and the consumer-owned detach policy (phux-4r1) decided to
    /// leave (nothing left to render or route input to).
    pub(super) exit: bool,
    /// phux-i0e8.2.2: WHY the loop is exiting, when `exit` is `true`.
    /// `Some(LastPaneClosed { .. })` is set ONLY by the `TerminalClosed`
    /// arm when the fold emptied the workspace, carrying the dead pane's
    /// exit status so the CLI can explain the exit on the cooked terminal
    /// after teardown. `None` with `exit: true` means a plain detach
    /// (server `DETACHED`); the driver folds it to [`AttachEnd::Detached`].
    pub(super) exit_reason: Option<AttachEnd>,
    /// `true` ⇒ ATTACHED just landed; the driver should emit
    /// `GET_METADATA` + `SUBSCRIBE_METADATA` for the layout key so
    /// other clients' mutations broadcast back to us (ADR-0019).
    pub(super) subscribe_layout: bool,
    /// `true` ⇒ the workspace was replaced by a server-side layout
    /// envelope (`MetadataValue` reply or `MetadataChanged` broadcast).
    /// The driver triggers a full repaint of the multi-pane composition.
    pub(super) layout_replaced: bool,
    /// Layout leaves newly discovered from peer metadata. The driver attaches
    /// each Terminal so its authoritative snapshot/output stream can populate
    /// a pane slot; this does not alter client-local focus.
    pub(super) attach_panes: Vec<TerminalId>,
    /// phux-4li.12: `true` ⇒ the server-side frame mutated layout in
    /// a way the *local* client originated (split landed, kill folded);
    /// the driver should broadcast the new envelope via
    /// `SET_METADATA` so sibling clients reconcile.
    pub(super) emit_set_metadata: bool,
    /// phux-tnh: `true` ⇒ a pane lifecycle event (close/spawn) changed
    /// surviving panes' dimensions. The driver must diff the new layout
    /// against the pre-frame rects and emit a `TERMINAL_RESIZE` per
    /// changed leaf so the server reflows each PTY (TIOCSWINSZ) — without
    /// this the survivor of a close keeps its old small winsize and the
    /// shell never redraws to fill the freed space. Set ONLY by the
    /// `TerminalClosed`/`TerminalSpawned` arms, not by the broader
    /// `layout_replaced` reconcile/broadcast paths (which already sized
    /// their panes and would otherwise thrash on attach).
    pub(super) reflow_panes: bool,
    /// Exact cumulative StateSync acknowledgement emitted by the session kernel.
    pub(super) ack: Option<(TerminalId, StreamId, BootstrapId, u64)>,
    /// The engine rejected a generation after emitting a typed resync status.
    ///
    /// The driver issues a fresh in-connection ATTACH while this outcome leaves
    /// the frozen published replica visible.
    pub(super) resync_required: bool,
    /// Pull the next opaque native history page after READY or a prior page.
    pub(super) history_request: Option<(TerminalId, StreamId, BootstrapId, bytes::Bytes, u32, u32)>,
    /// Exact terminal-engine response writes to forward on the ordered PTY lane.
    pub(super) pty_writes: Vec<(TerminalId, Vec<u8>)>,
    /// phux-4li.20: `Some((sessions, focused))` ⇒ ATTACHED just landed
    /// and carried the server's full session graph. The driver caches
    /// it so the `<leader> a` session picker can list the other
    /// sessions without a follow-up request/response frame — the
    /// `ATTACHED` snapshot is already authoritative at attach time (SPEC
    /// §13). Set ONLY by the `Attached` arm.
    pub(super) sessions: Option<(Vec<SessionInfo>, SessionId)>,
    /// ADR-0033: `Some(id)` ⇒ ATTACHED carried this client's own server-assigned
    /// `ClientId`. The driver caches it to tell "you have the wheel" from
    /// another client holding it when rendering the supervisory badge. Set ONLY
    /// by the `Attached` arm.
    pub(super) own_client_id: Option<ClientId>,
    /// ADR-0033 / phux-foz.1: `true` ⇒ an agent event updated a pane's
    /// lifecycle, input-lease holder (`TerminalControl`), or asked-attention
    /// flag (ADR-0035 `Asked`), so the driver must repaint the chrome
    /// (supervisory badge, attention hint, window-tab markers) even though no
    /// grid content changed. Set by the `Event`, bootstrap, and
    /// `TerminalOutput` arms when the applied bytes moved the pane's OSC 0/2
    /// title — the title feeds the window-tab labels and
    /// the sidebar's agents section (the only identity signal a plain
    /// `claude`/`codex` pane emits), and title bytes arrive on the ordinary
    /// content path that otherwise never refreshes the chrome.
    pub(super) chrome_dirty: bool,
    /// ADR-0040: `true` ⇒ a `phux.agent/v1` record changed for some pane
    /// (a `GET_METADATA` reply or a `METADATA_CHANGED` broadcast). Window
    /// labels derive from it, so the driver refreshes the window chrome
    /// (tab strip + sidebar) and repaints. Set ONLY by the
    /// `MetadataValue` / `MetadataChanged` arms.
    pub(super) agent_meta_changed: bool,
    /// phux-p4vp: per-pane working directories carried by the `ATTACHED`
    /// snapshot (`TerminalInfo::cwd`). The driver folds these into its
    /// pane-cwd index, from which the sidebar's branch line is derived
    /// client-side (see `crate::vcs`). Set ONLY by the `Attached` arm;
    /// empty otherwise.
    pub(super) pane_cwds: Vec<(TerminalId, String)>,
    /// phux-foz.5: `true` ⇒ a subscribed `phux.config.reload/v1`
    /// doorbell rang (a `phux config reload` from some shell). The driver
    /// re-runs its layered config loader and swaps its config-derived
    /// state in place, exactly as for the `reload-config` action; on a
    /// failed re-read it keeps the previous config and surfaces the
    /// error. Set ONLY by the `MetadataChanged` arm; tombstones do not
    /// set it.
    pub(super) config_reload: bool,
    /// phux-i0e8.2.1: transient status-bar notices raised by this frame,
    /// drained by the driver into the painter's newest-wins notice slot
    /// (`StatusBarPainter::set_notice`) right after the dispatch returns.
    /// Producers today: a focused-pane input-authority (`TerminalControl`)
    /// holder transition, and a degraded-federation push (an uncorrelated
    /// `ERROR { SATELLITE_UNREACHABLE }`). Empty on every other frame.
    pub(super) notices: Vec<Notice>,
}

/// Payload-free label for the inbound `FrameKind` — the `kind` field on
/// the per-frame dispatch span. Keeps the trace line small and free of
/// content bytes / session names; the heavy content frames additionally
/// record `terminal_id` / `seq` / `bytes`. `FrameKind` is large and
/// `#[non_exhaustive]`, so this covers the S->C arms this handler acts on
/// and folds the rest into `"other"`.
const fn frame_kind_label(frame: &FrameKind) -> &'static str {
    match frame {
        FrameKind::Attached { .. } => "attached",
        FrameKind::BootstrapBegin { .. } => "bootstrap_begin",
        FrameKind::BootstrapChunk { .. } => "bootstrap_chunk",
        FrameKind::BootstrapReady { .. } => "bootstrap_ready",
        FrameKind::HistoryPage { .. } => "history_page",
        FrameKind::HistoryTombstone { .. } => "history_tombstone",
        FrameKind::HistoryRejected { .. } => "history_rejected",
        FrameKind::BootstrapTombstone { .. } => "bootstrap_tombstone",
        FrameKind::AttachReady { .. } => "attach_ready",
        FrameKind::TerminalOutput { .. } => "terminal_output",
        FrameKind::Detached => "detached",
        FrameKind::Bell { .. } => "bell",
        FrameKind::MetadataValue { .. } => "metadata_value",
        FrameKind::MetadataChanged { .. } => "metadata_changed",
        FrameKind::TerminalSpawned { .. } => "terminal_spawned",
        FrameKind::TerminalClosed { .. } => "terminal_closed",
        _ => "other",
    }
}

/// phux-i0e8.2.1: text for the focused pane's input-authority notice.
///
/// The transient counterpart of the persistent `WHEEL:*` badge
/// (ADR-0033): the badge shows who holds the wheel; this line calls out
/// the transition itself. The client-id spelling (`c<N>`) matches the
/// badge's, so the two surfaces read as one vocabulary.
fn input_authority_notice(holder: Option<ClientId>) -> String {
    holder.map_or_else(
        || "input: wheel released".to_owned(),
        |id| format!("input: c{} took the wheel", id.get()),
    )
}

/// phux-i0e8.2.2: human phrase for a `TERMINAL_CLOSED` exit status.
///
/// The wire carries `Some(n)` for a plain `_exit(n)` and `None` for
/// signal kills / unknown causes (frame.rs `TerminalClosed`). One
/// spelling shared by the survivor notice and the last-pane exit
/// explanation, so both surfaces read as one vocabulary.
pub(super) fn describe_exit(exit_status: Option<i32>) -> String {
    exit_status.map_or_else(
        || "killed (signal or unknown)".to_owned(),
        |code| format!("exited {code}"),
    )
}

/// phux-i0e8.2.2: user-facing name for a pane in a status-bar notice.
///
/// A local terminal reads `pane N`; a federation satellite's pane keeps
/// its host tag (`pane host/N`) so the notice does not alias two panes
/// with the same peer-local id.
fn pane_label(id: &TerminalId) -> String {
    match id {
        TerminalId::Local { id } => format!("pane {id}"),
        TerminalId::Satellite { host, id } => format!("pane {host}/{id}"),
    }
}

/// Process one server-to-client frame. Returns a [`FrameOutcome`]
/// describing any follow-up the async driver needs to perform.
///
/// `status_bar` is `Option<&mut StatusBarPainter>` so an attach with no
/// configured widgets pays nothing for the chrome path. `viewport_dims`
/// is `(cols, rows)` of the outer terminal — used by the painter to
/// pick the bottom row.
#[allow(clippy::too_many_arguments)] // arg list bundles status-bar + predict state; follow-up to refactor into a context struct
#[allow(
    clippy::too_many_lines,
    reason = "phux-4li.5 added L3 reconcile branches; refactor with the status-bar arg-list cleanup"
)]
#[derive(Default)]
struct KernelRoute {
    ack: Option<(TerminalId, StreamId, BootstrapId, u64)>,
    history_request: Option<(TerminalId, StreamId, BootstrapId, bytes::Bytes, u32, u32)>,
    pty_writes: Vec<(TerminalId, Vec<u8>)>,
    damaged: HashSet<TerminalId>,
    resync_required: bool,
    ignored: bool,
    failed: Option<String>,
}
impl KernelRoute {
    fn damaged(&self, terminal_id: &TerminalId) -> bool {
        self.damaged.contains(terminal_id)
    }
}

fn history_unavailable_reason(reason: HistoryTombstoneReason) -> Option<HistoryUnavailableReason> {
    Some(match reason {
        HistoryTombstoneReason::Stale => HistoryUnavailableReason::Stale,
        HistoryTombstoneReason::Pruned => HistoryUnavailableReason::Pruned,
        HistoryTombstoneReason::Reset => HistoryUnavailableReason::Reset,
        HistoryTombstoneReason::Resize => HistoryUnavailableReason::Resize,
        HistoryTombstoneReason::Expired => HistoryUnavailableReason::Expired,
        HistoryTombstoneReason::Released => HistoryUnavailableReason::Released,
        HistoryTombstoneReason::Limit => HistoryUnavailableReason::Limit,
        HistoryTombstoneReason::CodecFailure => HistoryUnavailableReason::CodecFailure,
        _ => return None,
    })
}

fn history_rejection_reason(
    reason: HistoryRejectionReason,
) -> Option<KernelHistoryRejectionReason> {
    Some(match reason {
        HistoryRejectionReason::ZeroLimit => KernelHistoryRejectionReason::ZeroLimit,
        HistoryRejectionReason::TooSmall => KernelHistoryRejectionReason::TooSmall,
        HistoryRejectionReason::Busy => KernelHistoryRejectionReason::Busy,
        _ => return None,
    })
}

fn route_engine_frame(
    frame: &FrameKind,
    kernel: &mut super::driver::AttachKernel,
    effects: &mut KernelEffectBuffer,
) -> KernelRoute {
    let terminals;
    let input = match frame {
        FrameKind::Attached {
            attach_id,
            snapshot,
            ..
        } => {
            terminals = snapshot
                .panes
                .iter()
                .map(|pane| pane.id.clone())
                .collect::<Vec<_>>();
            Some(KernelInput::AttachStarted {
                attach_id: *attach_id,
                terminals: &terminals,
            })
        }
        FrameKind::AttachReady { attach_id } => Some(KernelInput::AttachReady {
            attach_id: *attach_id,
        }),
        FrameKind::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            profile,
            cols,
            rows,
            base_seq,
        } => Some(KernelInput::BootstrapBegin {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            profile: *profile,
            geometry: CanonicalGeometry {
                cols: *cols,
                rows: *rows,
            },
            base_seq: *base_seq,
        }),
        FrameKind::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        } => Some(KernelInput::BootstrapChunk {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            chunk_seq: *chunk_seq,
            payload,
        }),
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor,
        } => Some(KernelInput::BootstrapReady {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            history_cursor: history_cursor.as_deref(),
        }),
        FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            rows,
            page_seq,
            cursor,
            next_cursor,
            payload,
        } => Some(KernelInput::HistoryPage {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            rows: *rows,
            page_seq: *page_seq,
            payload,
            cursor,
            next_cursor: next_cursor.as_deref(),
        }),
        FrameKind::HistoryTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
        } => Some(KernelInput::HistoryTombstone {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            cursor,
            reason: match history_unavailable_reason(*reason) {
                Some(reason) => reason,
                None => {
                    return KernelRoute {
                        failed: Some("unsupported history tombstone reason".to_owned()),
                        ..KernelRoute::default()
                    };
                }
            },
        }),
        FrameKind::HistoryRejected {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
            required_bytes,
            required_rows,
        } => Some(KernelInput::HistoryRejected {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            cursor,
            reason: match history_rejection_reason(*reason) {
                Some(reason) => reason,
                None => {
                    return KernelRoute {
                        failed: Some("unsupported history rejection reason".to_owned()),
                        ..KernelRoute::default()
                    };
                }
            },
            required_bytes: *required_bytes,
            required_rows: *required_rows,
        }),
        FrameKind::TerminalOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        } => Some(KernelInput::TerminalOutput {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            seq: *seq,
            payload: bytes,
        }),
        FrameKind::BootstrapTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            reason,
            last_valid_seq,
        } => Some(KernelInput::Tombstone {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            reason: *reason,
            last_valid_seq: *last_valid_seq,
        }),
        FrameKind::TerminalClosed { terminal_id, .. } => {
            Some(KernelInput::TerminalClosed { terminal_id })
        }
        _ => None,
    };
    let Some(input) = input else {
        return KernelRoute::default();
    };

    effects.clear();
    let result = kernel.update(input, effects);
    let resync_required = result.is_err()
        && effects.as_slice().iter().any(|effect| {
            matches!(
                effect,
                KernelEffect::Status(
                    phux_client_core::session::KernelStatus::ResyncRequired { .. }
                )
            )
        });
    let ignored = matches!(
        &result,
        Err(phux_client_core::session::KernelError::RetiredGeneration { .. })
    );
    let failed = match result {
        Ok(()) => None,
        Err(_) if resync_required || ignored => None,
        Err(error) => Some(error.to_string()),
    };
    let mut route = KernelRoute {
        resync_required,
        ignored,
        failed,
        ..KernelRoute::default()
    };
    for effect in effects.as_slice() {
        match effect {
            KernelEffect::Send(KernelSend::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            }) => {
                route.ack = Some((terminal_id.clone(), *stream_id, *bootstrap_id, *seq));
            }
            KernelEffect::Send(KernelSend::HistoryRequest {
                key,
                cursor,
                max_bytes,
                max_rows,
            }) => {
                route.history_request = Some((
                    key.terminal_id.clone(),
                    key.stream_id,
                    key.bootstrap_id,
                    bytes::Bytes::from(cursor.clone()),
                    *max_bytes,
                    *max_rows,
                ));
            }
            KernelEffect::Send(KernelSend::PtyWrite { terminal_id, bytes }) => {
                route.pty_writes.push((terminal_id.clone(), bytes.clone()));
            }
            KernelEffect::Damage(damage) => {
                route.damaged.insert(damage.terminal_id.clone());
            }
            KernelEffect::Status(status) => {
                tracing::warn!(?status, "session kernel status");
            }
            KernelEffect::Job(job) => {
                tracing::debug!(?job, "session kernel cooperative job");
            }
            KernelEffect::Send(send) => {
                tracing::warn!(?send, "unexpected synchronous engine send");
            }
        }
    }
    route
}

#[allow(
    clippy::cognitive_complexity,
    reason = "phux-4li.12 adds TerminalSpawned/TerminalClosed branches with full SpawnError matching; per-frame dispatcher is intentionally flat"
)]
pub(super) fn handle_server_frame<W: super::RenderSink>(
    engine_kernel: &mut super::driver::AttachKernel,
    kernel_effects: &mut KernelEffectBuffer,
    out: &mut W,
    frame: FrameKind,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    workspace: &mut Workspace,
    focused_pane: &mut Option<TerminalId>,
    // phux-x2hm: the driver's pane-zoom state. RENDER/REFLOW geometry reads go
    // through `Workspace::render_window(zoomed)` so a zoomed pane paints to the
    // full window and non-zoomed panes (absent from the synthetic single-leaf
    // layout) correctly do not paint. A `TerminalSpawned`-ok split clears this
    // (`*zoomed = None`) so a new pane un-zooms, matching tmux. Mutation/input
    // reads (focus reconcile) keep using the REAL `active_window`.
    zoomed: &mut Option<TerminalId>,
    session_name: &mut String,
    status_bar: Option<&mut StatusBarPainter>,
    // phux-4h5a: the window-sidebar reservation, threaded identically to
    // `status_bar` so every layout site in this dispatcher tiles panes into
    // the SAME inset content rect the driver paints + reflows against. `None`
    // (sidebar disabled, the default) makes `content_rect` the full pane
    // viewport, so the whole dispatcher stays byte-identical to the
    // pre-sidebar path.
    sidebar: Option<SidebarReservation>,
    viewport_dims: (u16, u16),
    predict: &mut PredictionState,
    overlay: &Overlay,
    pending_layout_request: Option<u32>,
    pending_splits: &mut HashMap<u32, PendingSplit>,
    pending_windows: &mut HashMap<u32, PendingWindow>,
    // phux-i0e8.2.2: Terminals whose close THIS client asked for
    // (kill-pane / kill-window soft-kill dispatch). The `TerminalClosed`
    // arm drains the marker and suppresses the pane-exit notice for an
    // expected close — the user killed it; telling them it died is noise.
    expected_closes: &mut HashSet<TerminalId>,
    // ADR-0040: the driver-held `phux.agent/v1` index. The MetadataValue /
    // MetadataChanged arms decode agent records into it; the driver reads
    // it when composing window labels.
    agent_meta: &mut AgentMetaIndex,
    // phux-5ke.4: when `true` an overlay is on top; pane libghostty
    // mirrors keep ingesting `vt_write` (per ADR-0013) but stdout
    // flushes (render_at, bar paint, predict-overlay paint) are
    // suppressed so the modal doesn't get scribbled over. The driver
    // triggers a full repaint on overlay dismiss.
    overlay_active: bool,
    // phux-jhv8: when `true` this frame is an earlier member of a coalesced
    // burst — a later frame in the same drain targets this pane, so its
    // libghostty mirror still ingests `vt_write` (state stays correct) but the
    // stdout paint (render_at, bar, predict-overlay, reconcile) is suppressed.
    // The driver passes `defer_paint = false` for each pane's LAST frame in the
    // burst, so every touched pane settles exactly once instead of repainting
    // on every intermediate redraw. Same vt_write-but-no-paint contract as
    // `overlay_active`, minus the modal semantics.
    defer_paint: bool,
) -> Result<FrameOutcome, AttachError> {
    let apply_span =
        matches!(frame, FrameKind::TerminalOutput { .. }).then(|| tracing::debug_span!("vt_apply"));
    let apply_guard = apply_span.as_ref().map(tracing::Span::enter);
    let kernel_route = route_engine_frame(&frame, engine_kernel, kernel_effects);
    drop(apply_guard);
    if let Some(error) = kernel_route.failed.as_ref() {
        return Err(AttachError::Protocol(format!(
            "session kernel rejected {}: {error}",
            frame_kind_label(&frame),
        )));
    }
    if kernel_route.resync_required {
        return Ok(FrameOutcome {
            resync_required: true,
            ..FrameOutcome::default()
        });
    }
    if kernel_route.ignored {
        return Ok(FrameOutcome::default());
    }
    // Per-inbound-frame dispatch span (debug; off under the default
    // `phux=info` filter). Content-frame CLOSE duration is client apply+paint
    // cost, while identifiers and payload sizes are recorded in their arms.
    // below. Declared `Empty` so they exist for later `record`.
    let frame_span = tracing::debug_span!(
        "handle_server_frame",
        kind = frame_kind_label(&frame),
        terminal_id = tracing::field::Empty,
        seq = tracing::field::Empty,
        bytes = tracing::field::Empty,
    )
    .entered();
    match frame {
        FrameKind::Attached {
            attach_id: _,
            snapshot,
            initial_client_id,
        } => {
            // Capture the initial focused pane so subsequent INPUT_* frames
            // know where to route.
            let bootstrap = snapshot.focused_pane.clone();
            tracing::debug!(
                terminal_id = ?bootstrap,
                "ATTACHED: seeding focused_pane from snapshot"
            );
            *focused_pane = Some(bootstrap.clone());
            // phux-4li.4: seed the workspace with a single window holding
            // one leaf so the existing single-pane render path keeps
            // working. The L3 metadata-fetch path replaces this with the
            // server-stored layout (possibly multi-window) when present.
            *workspace = Workspace::single(bootstrap.clone());
            // Seed client-side mirrors at their server-advertised sizes
            // before any TERMINAL_OUTPUT can race ahead of the per-pane
            // bootstrap transcript. VT interpretation is geometry-sensitive;
            // starting at 80x24 and resizing later corrupts wraps, clips,
            // and absolute cursor movement for wider/taller viewports.
            for pane in &snapshot.panes {
                if let std::collections::hash_map::Entry::Vacant(v) = panes.entry(pane.id.clone()) {
                    let slot = v.insert(PaneSlot::new_with_size(pane.cols, pane.rows)?);
                    // phux-foz.4: seed the pane's cwd from the snapshot (the
                    // spawn cwd); `cwd_changed` events refine it live.
                    slot.cwd.clone_from(&pane.cwd);
                }
            }
            // phux-p4vp: hand the per-pane cwds up to the driver so the
            // sidebar can derive each window's VCS branch client-side.
            let pane_cwds: Vec<(TerminalId, String)> = snapshot
                .panes
                .iter()
                .filter_map(|p| p.cwd.clone().map(|cwd| (p.id.clone(), cwd)))
                .collect();
            // Ensure the focused pane has a slot even if an older server's
            // ATTACHED graph omitted it. Fall back to the current pane
            // viewport (the same dimensions used for rendering) rather
            // than the historical 80x24 placeholder.
            if let std::collections::hash_map::Entry::Vacant(v) = panes.entry(bootstrap) {
                let content = content_rect(
                    viewport_dims,
                    status_bar.as_ref().map(|p| p.position()),
                    sidebar,
                );
                v.insert(PaneSlot::new_with_size(content.w, content.h)?);
            }
            // phux-17u: stash the session name for the status-bar
            // `WidgetContext`. The snapshot carries `sessions:
            // Vec<SessionInfo>` plus `focused_session`; the name is the
            // `SessionInfo` whose `id` matches the focused session. The
            // server populates this from `Session::name` in
            // `build_session_snapshot`. Falls back to empty if the
            // focused session somehow isn't in the list (shouldn't
            // happen — the focused session is always one of them).
            *session_name = focused_session_name(&snapshot);
            // phux-4li.20: hand the driver the full session graph so the
            // `<leader> a` session picker can list peer sessions. The
            // snapshot is the authoritative session list at attach time;
            // a dedicated request/response frame would be redundant.
            let session_cache = (snapshot.sessions.clone(), snapshot.focused_session);
            // `ATTACHED` per SPEC §13 carries the session/window/pane
            // graph; the per-pane initial cells arrive separately through each
            // bootstrap transcript.
            //
            // phux-4li.5: signal the driver to emit GET_METADATA and
            // SUBSCRIBE_METADATA for the layout key so we (a) reconcile
            // against a persisted layout from a previous session and
            // (b) receive METADATA_CHANGED broadcasts from sibling
            // clients (ADR-0019 decision 2).
            Ok(FrameOutcome {
                subscribe_layout: true,
                sessions: Some(session_cache),
                // ADR-0033: cache our own ClientId so the supervisory badge can
                // distinguish "you hold the wheel" from another client.
                own_client_id: Some(initial_client_id),
                pane_cwds,
                ..FrameOutcome::default()
            })
        }
        FrameKind::BootstrapBegin {
            terminal_id,
            cols,
            rows,
            ..
        } => {
            let slot = match panes.entry(terminal_id) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(PaneSlot::new_with_size(cols, rows)?)
                }
            };
            slot.geometry = (cols.max(1), rows.max(1));
            Ok(FrameOutcome::default())
        }
        FrameKind::BootstrapChunk {
            terminal_id,
            payload,
            ..
        } => {
            frame_span.record("terminal_id", tracing::field::debug(&terminal_id));
            frame_span.record("bytes", payload.len());
            Ok(FrameOutcome {
                pty_writes: kernel_route.pty_writes,
                ..FrameOutcome::default()
            })
        }
        FrameKind::BootstrapReady { terminal_id, .. } => {
            frame_span.record("terminal_id", tracing::field::debug(&terminal_id));
            let terminal = super::driver::published_terminal(engine_kernel, &terminal_id)
                .ok_or_else(|| {
                    AttachError::Protocol(format!(
                        "BOOTSTRAP_READY did not publish {terminal_id:?}"
                    ))
                })?;
            let slot = panes
                .get_mut(&terminal_id)
                .ok_or_else(|| AttachError::Protocol("READY without pane slot".to_owned()))?;
            let title_changed = slot.title_changed(terminal);
            slot.update_sync_output(terminal, tokio::time::Instant::now());
            let damaged = kernel_route.damaged(&terminal_id);
            Ok(FrameOutcome {
                layout_replaced: damaged,
                chrome_dirty: damaged && title_changed,
                history_request: kernel_route.history_request,
                pty_writes: kernel_route.pty_writes,
                ..FrameOutcome::default()
            })
        }
        FrameKind::HistoryPage { .. }
        | FrameKind::HistoryTombstone { .. }
        | FrameKind::HistoryRejected { .. } => Ok(FrameOutcome {
            history_request: kernel_route.history_request,
            pty_writes: kernel_route.pty_writes,
            ..FrameOutcome::default()
        }),
        FrameKind::AttachReady { .. } => Ok(FrameOutcome {
            layout_replaced: !kernel_route.damaged.is_empty(),
            ..FrameOutcome::default()
        }),
        FrameKind::TerminalOutput {
            terminal_id,
            stream_id: _,
            bootstrap_id: _,
            seq,
            bytes,
        } => {
            let damaged = kernel_route.damaged(&terminal_id);
            let ack = kernel_route.ack;
            let pty_writes = kernel_route.pty_writes;
            // Correlate this apply: which pane, which seq, how many bytes.
            // The span's CLOSE duration is the per-frame client paint cost
            // (vt_write + render_at for the focused pane) — the headline
            // client lag signal a trace reader greps `handle_server_frame`
            // with `kind=terminal_output` for.
            frame_span.record("terminal_id", tracing::field::debug(&terminal_id));
            frame_span.record("seq", seq);
            frame_span.record("bytes", bytes.len());
            // The kernel already applied these bytes to the published
            // libghostty terminal, including for an off-screen pane. Refresh
            // pane metadata from that authoritative terminal before deciding
            // whether the aggregate attach barrier permits paint damage.
            // A pre-barrier OSC title must update chrome caches even though
            // its visible repaint remains suppressed until ATTACH_READY.
            let terminal = super::driver::published_terminal(engine_kernel, &terminal_id)
                .ok_or_else(|| {
                    AttachError::Protocol(format!(
                        "TERMINAL_OUTPUT targeted unpublished {terminal_id:?}"
                    ))
                })?;
            let bar = status_bar.as_ref().map(|p| p.position());
            let content = content_rect(viewport_dims, bar, sidebar);
            let initial_dims = workspace
                .render_window(zoomed.as_ref())
                .and_then(|ls| {
                    super::multi_pane::compute_layout_in(ls.as_ref(), content, viewport_dims)
                        .rects
                        .get(&terminal_id)
                        .map(|r| (r.w, r.h))
                })
                .unwrap_or((content.w, content.h));
            let is_focused = focused_pane.as_ref() == Some(&terminal_id);
            let slot = match panes.entry(terminal_id.clone()) {
                std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(PaneSlot::new_with_size(initial_dims.0, initial_dims.1)?)
                }
            };
            let title_changed = slot.title_changed(terminal);
            let sync_output_active = slot.update_sync_output(terminal, tokio::time::Instant::now());
            if !damaged {
                return Ok(FrameOutcome {
                    ack,
                    pty_writes,
                    ..FrameOutcome::default()
                });
            }
            // The libghostty mirror is now warm even for panes in a
            // non-active window (off-screen invariant). Rendering only
            // applies to the active window's composition; if there's no
            // active window there's nothing on-screen to repaint.
            // phux-x2hm: render against the zoom-honoring view so a zoomed
            // pane paints to the whole window and the others (absent from the
            // synthetic single-leaf layout) get no rect and so do not paint.
            let Some(active_ls) = workspace.render_window(zoomed.as_ref()) else {
                return Ok(FrameOutcome {
                    ack,
                    pty_writes,
                    chrome_dirty: title_changed,
                    ..FrameOutcome::default()
                });
            };
            let active_ls = active_ls.as_ref();
            if is_focused
                && !overlay_active
                && !defer_paint
                && !sync_output_active
                && let Some(fid) = focused_pane.as_ref()
            {
                // phux-flywheel: the paint trigger — render the focused
                // pane (this enters `paint_full_frame`'s span inside
                // `paint_focused_pane`), reconcile predictions, repaint the
                // bar. Its OWN child span isolates paint cost from the
                // `vt_apply` above so a trace shows apply-ms vs paint-ms
                // separately. Debug-level + lazy `rows` field ⇒ free at the
                // default filter.
                let _paint_trigger =
                    tracing::debug_span!("paint_trigger", rows = viewport_dims.1).entered();
                let bar = status_bar.as_ref().map(|p| p.position());
                let _ = paint_focused_pane(
                    out,
                    active_ls,
                    panes,
                    engine_kernel,
                    fid,
                    viewport_dims,
                    bar,
                    sidebar,
                    false,
                );
                // The reconcile + overlay work entirely in PANE-LOCAL
                // coordinates (predictions are pane-local; the cell reader
                // indexes the pane's own grid). `focused_cursor` (outer) is
                // kept only for the host-cursor restore in the bar paint.
                let (focused_cursor, focused_cursor_local, pane_origin) =
                    panes.get(fid).map_or((None, None, (0, 0)), |s| {
                        (
                            s.renderer.last_cursor(),
                            s.renderer.last_cursor_local(),
                            s.renderer.last_origin(),
                        )
                    });
                // Per-cell match reconcile (phux-9gw.1.1): walk pending
                // predictions against the freshly painted cell grid;
                // confirmed predictions drop, contradictions drop their
                // suffix, predictions still ahead of confirmed state
                // stay alive. See [`crate::predict`] for the truth table.
                if let Some((row, col)) = focused_cursor_local {
                    let _stats = reconcile_terminal_output_per_cell(predict, row, col, |r, c| {
                        panes.get_mut(fid).and_then(|s| {
                            // Read the full grapheme cluster, not just the
                            // base scalar, so multi-codepoint Insert
                            // predictions (flag emoji, ZWJ sequences, base
                            // plus combining marks) reconcile against the
                            // whole painted cluster (phux-9gw.1.6).
                            s.renderer
                                .read_grapheme_string_at(terminal, r, c)
                                .ok()
                                .flatten()
                        })
                    });
                } else {
                    // Cursor hidden — we can't anchor reliably; fall
                    // back to the wholesale drain. Rare path (programs
                    // that hide the cursor before a redraw).
                    predict.clear();
                }
                // Overlay paints any predictions still alive (the tail
                // of a partial confirmation), shifted by the focused pane's
                // outer origin. On a fully-drained queue this is a no-op.
                let _ = overlay.render(predict, pane_origin, out);
                // phux-9xn: compute the focused pane's Rect origin so
                // the bar paint can park the cursor there if
                // `last_cursor` is None. Without this fallback the
                // bar's final write leaves the host terminal cursor
                // at bottom-right.
                let content = content_rect(viewport_dims, bar, sidebar);
                let fallback_origin =
                    super::multi_pane::compute_layout_in(active_ls, content, viewport_dims)
                        .rects
                        .get(fid)
                        .map(|r| (r.x, r.y))
                        .or(Some((0, 0)));
                paint_bar_after_pane(
                    status_bar,
                    out,
                    viewport_dims,
                    sidebar,
                    session_name,
                    focused_cursor,
                    fallback_origin,
                    // Hot path: pane render stays above the bar row, so the
                    // painter's cache makes an unchanged bar a zero-byte
                    // no-op (incremental-paint win).
                    false,
                );
            } else if !overlay_active && !defer_paint && !sync_output_active {
                // phux-2x9: repaint a NON-focused pane on its own output
                // so it isn't visually frozen — output (and the
                // post-split/resize resync snapshot) must show without
                // the user focusing the pane. render_at is dirty-tracked,
                // so steady-state output only repaints changed rows. After
                // painting into this pane's rect we restore the focused
                // pane's cursor so the host cursor stays where the user is
                // typing.
                let bar = status_bar.as_ref().map(|p| p.position());
                let content = content_rect(viewport_dims, bar, sidebar);
                let rects =
                    super::multi_pane::compute_layout_in(active_ls, content, viewport_dims).rects;
                if let Some(rect) = rects.get(&terminal_id).copied() {
                    if let Some(slot) = panes.get_mut(&terminal_id) {
                        // phux-foz.11: letterbox like every other paint path.
                        // An undersized mirror (resize handshake in flight)
                        // painted incrementally at the rect origin here, while
                        // `paint_full_frame` centres the same mirror — dirty
                        // rows then land offset from the full-frame rows and
                        // the pane shows doubled text until a full repaint.
                        // Mirror >= rect degrades to the prior `render_at`.
                        let mirror = super::paint::mirror_dims(terminal, rect);
                        let _ = slot.renderer.render_at_letterboxed(
                            terminal,
                            out,
                            (rect.x, rect.y),
                            (rect.w, rect.h),
                            mirror,
                            false,
                        );
                    }
                    // Restore the focused pane's cursor: the render above
                    // left the host cursor inside the non-focused pane.
                    let focused_cursor = focused_pane
                        .as_ref()
                        .and_then(|fid| panes.get(fid))
                        .and_then(|s| s.renderer.last_cursor());
                    if status_bar.is_some() {
                        let fallback = focused_pane
                            .as_ref()
                            .and_then(|fid| rects.get(fid))
                            .map(|r| (r.x, r.y));
                        paint_bar_after_pane(
                            status_bar,
                            out,
                            viewport_dims,
                            sidebar,
                            session_name,
                            focused_cursor,
                            fallback,
                            // Non-focused pane render stays above the bar
                            // row; cache decides whether to re-emit.
                            false,
                        );
                    } else if let Some((row, col)) = focused_cursor {
                        let _ = write!(
                            out,
                            "\x1b[{};{}H\x1b[?25h",
                            row.saturating_add(1),
                            col.saturating_add(1)
                        );
                        let _ = out.flush();
                    } else {
                        let _ = out.flush();
                    }
                }
            }
            Ok(FrameOutcome {
                ack,
                chrome_dirty: title_changed,
                pty_writes,
                ..FrameOutcome::default()
            })
        }
        FrameKind::BootstrapTombstone { .. } => Ok(FrameOutcome::default()),
        FrameKind::Detached => Ok(FrameOutcome {
            exit: true,
            ..FrameOutcome::default()
        }),
        FrameKind::Bell { .. } => {
            // Forward bell to the outer terminal. The user's terminal
            // emulator decides whether to render visually, audibly, or
            // not at all. Routed through the injected sink so a headless
            // capture sees the BEL too (an agent can observe `\x07`).
            let _ = actions::write_bell(out);
            Ok(FrameOutcome::default())
        }
        // phux-4li.5: reconcile-on-attach reply path. The driver sends
        // `GET_METADATA { request_id }` immediately after ATTACHED;
        // the server replies with `MetadataValue { request_id, value }`.
        // Match by id, decode the layout envelope, and adopt its topology
        // while preserving this client's valid active window and per-window
        // focus. `value: None` means "no persisted layout" — keep the
        // single-pane bootstrap untouched.
        FrameKind::MetadataValue { request_id, value } => {
            // ADR-0040: a pending per-Terminal `phux.agent/v1` GET reply.
            // `value: None` (key absent) clears any stale record.
            if let Some(terminal) = agent_meta.pending.remove(&request_id) {
                let changed = agent_meta.apply(&terminal, value.as_deref());
                if changed {
                    note_agent_change(panes, focused_pane.as_ref(), &terminal);
                }
                return Ok(FrameOutcome {
                    agent_meta_changed: changed,
                    ..FrameOutcome::default()
                });
            }
            if Some(request_id) != pending_layout_request {
                tracing::debug!(
                    request_id,
                    "dropping MetadataValue with no matching pending request"
                );
                return Ok(FrameOutcome::default());
            }
            let Some(bytes) = value else {
                return Ok(FrameOutcome::default());
            };
            match Workspace::decode_cbor(&bytes) {
                Ok(new_ws) => {
                    let (reconciled, accepted) = reconcile_loaded_workspace_checked(
                        new_ws,
                        workspace,
                        focused_pane.as_ref(),
                        panes,
                    );
                    *workspace = reconciled;
                    let attach_panes = if accepted {
                        unknown_layout_leaves(workspace, panes)
                    } else {
                        Vec::new()
                    };
                    // Re-anchor the driver's focused-pane mirror onto the
                    // active window's client-local reconciled focus.
                    *focused_pane = workspace.active_window().and_then(|ls| ls.focus.clone());
                    Ok(FrameOutcome {
                        layout_replaced: true,
                        attach_panes,
                        ..FrameOutcome::default()
                    })
                }
                Err(err) => {
                    tracing::warn!(error = %err, "failed to decode persisted layout; keeping bootstrap");
                    Ok(FrameOutcome::default())
                }
            }
        }
        // phux-4li.5: broadcast reconcile. Another attached client
        // mutated `phux.tui.layout/v1`; decode + adopt topology + repaint.
        // ADR-0049: the sender's serialized focus is never authoritative.
        // Tombstones (`value: None`) are treated as "layout reset" —
        // fall back to the single-pane bootstrap so the next render
        // doesn't try to draw against a stale tree.
        FrameKind::MetadataChanged { scope, key, value } => {
            // ADR-0040: a `phux.agent/v1` broadcast for a subscribed pane.
            // A tombstone (`value: None`, the DELETE_METADATA path) clears
            // the record and the label falls back to the OSC title.
            if key == TERMINAL_AGENT_KEY {
                if let Scope::Terminal(terminal) = &scope {
                    let changed = agent_meta.apply(terminal, value.as_deref());
                    if changed {
                        note_agent_change(panes, focused_pane.as_ref(), terminal);
                    }
                    return Ok(FrameOutcome {
                        agent_meta_changed: changed,
                        ..FrameOutcome::default()
                    });
                }
                return Ok(FrameOutcome::default());
            }
            // phux-foz.5: the config-reload doorbell. Value bytes are an
            // opaque nonce (only there to defeat the server's equal-bytes
            // SET dedup); a tombstone is not a reload request.
            if key == CONFIG_RELOAD_KEY && matches!(scope, Scope::Global) {
                return Ok(FrameOutcome {
                    config_reload: value.is_some(),
                    ..FrameOutcome::default()
                });
            }
            if !is_layout_key(&scope, &key) {
                return Ok(FrameOutcome::default());
            }
            if let Some(bytes) = value {
                match Workspace::decode_cbor(&bytes) {
                    Ok(new_ws) => {
                        let (reconciled, accepted) = reconcile_loaded_workspace_checked(
                            new_ws,
                            workspace,
                            focused_pane.as_ref(),
                            panes,
                        );
                        *workspace = reconciled;
                        let attach_panes = if accepted {
                            unknown_layout_leaves(workspace, panes)
                        } else {
                            Vec::new()
                        };
                        *focused_pane = workspace.active_window().and_then(|ls| ls.focus.clone());
                        Ok(FrameOutcome {
                            layout_replaced: true,
                            attach_panes,
                            ..FrameOutcome::default()
                        })
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "broadcast layout decode failed; ignoring");
                        Ok(FrameOutcome::default())
                    }
                }
            } else {
                // Tombstone: layout reset. Fall back to single-pane
                // bootstrap (or empty if there's no focus to anchor on).
                *workspace = focused_pane
                    .clone()
                    .map_or_else(Workspace::default, Workspace::single);
                Ok(FrameOutcome {
                    layout_replaced: true,
                    ..FrameOutcome::default()
                })
            }
        }
        // phux-4li.12: split-pane reply path. Look up the parked
        // PendingSplit by request id; on Ok apply the split + seed the
        // new PaneSlot + broadcast the envelope. On Err log + bell.
        FrameKind::TerminalSpawned { request_id, result } => {
            // phux-4li.15: a parked new-window takes priority — its reply
            // opens a window on the spawned pane instead of splitting the
            // active one. Request ids are unique across both maps.
            if let Some(pending) = pending_windows.remove(&request_id) {
                return handle_window_spawned(
                    out,
                    workspace,
                    focused_pane,
                    panes,
                    &pending,
                    result,
                );
            }
            let Some(pending) = pending_splits.remove(&request_id) else {
                tracing::debug!(
                    request_id,
                    "stray TerminalSpawned with no matching pending split or window; ignoring",
                );
                return Ok(FrameOutcome::default());
            };
            match result {
                SpawnResult::Ok(new_id) => {
                    let Some(active_ls) = workspace.active_window_mut() else {
                        tracing::warn!("TerminalSpawned: no active window to apply split into");
                        let _ = actions::write_bell(out);
                        return Ok(FrameOutcome::default());
                    };
                    match apply_spawned_ok(active_ls, new_id.clone(), &pending) {
                        Ok(new_state) => {
                            *active_ls = new_state;
                            // phux-x2hm: a split un-zooms (tmux parity). The
                            // new pane needs its tile, and the reflow_panes
                            // diff below is taken against the now-cleared
                            // (real, tiled) view.
                            // phux-r82.7: unless the parked intent asked to
                            // zoom the spawned pane (`placement = "zoomed"`
                            // plugin panes) — then the new pane fills the
                            // window and un-zooming reveals it tiled beside
                            // its anchor.
                            *zoomed = if pending.zoom_on_spawn {
                                Some(new_id.clone())
                            } else {
                                None
                            };
                            // Seed pane metadata so the first bootstrap lands
                            // on a warm rendering slot. Vacant-or-occupied —
                            // never overwrite existing frontend metadata.
                            if let std::collections::hash_map::Entry::Vacant(v) =
                                panes.entry(new_id)
                            {
                                v.insert(PaneSlot::new()?);
                            }
                            // Move focus to the freshly spawned pane —
                            // tmux-compatible (apply_split already sets
                            // focus inside the returned state).
                            focused_pane.clone_from(
                                &workspace.active_window().and_then(|ls| ls.focus.clone()),
                            );
                            // Re-anchor predictive echo to the freshly
                            // focused pane (phux-7ry0). The split leaves the
                            // predict layer holding the previous pane's
                            // viewport + cursor; a keystroke before the new
                            // pane's first snapshot would otherwise echo at
                            // the old pane's coordinates (mid-screen ghost).
                            if let Some(fid) = focused_pane.as_ref() {
                                super::driver::reanchor_predict_to_pane(predict, panes, fid);
                            }
                            Ok(FrameOutcome {
                                layout_replaced: true,
                                emit_set_metadata: true,
                                // phux-tnh: the split shrank the sibling
                                // and added a leaf; emit per-leaf resizes
                                // so the server learns the real split dims
                                // instead of leaving panes at spawn size.
                                reflow_panes: true,
                                ..FrameOutcome::default()
                            })
                        }
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                terminal = ?new_id,
                                "apply_spawned_ok failed; dropping spawned terminal",
                            );
                            let _ = actions::write_bell(out);
                            Ok(FrameOutcome::default())
                        }
                    }
                }
                SpawnResult::Err(SpawnError::GroupNotFound) => {
                    // v0.1 clients only ever target DEFAULT_GROUP_ID,
                    // which the server always exposes; this branch
                    // means a server-side L2 invariant changed under
                    // us. Log loudly + bell.
                    tracing::warn!(
                        request_id,
                        "TerminalSpawned: server reports GroupNotFound for DEFAULT group",
                    );
                    let _ = actions::write_bell(out);
                    Ok(FrameOutcome::default())
                }
                SpawnResult::Err(SpawnError::SpawnFailed(reason)) => {
                    tracing::warn!(
                        request_id,
                        reason = %reason,
                        "TerminalSpawned: server-side spawn failed",
                    );
                    let _ = actions::write_bell(out);
                    Ok(FrameOutcome::default())
                }
                // SpawnError is #[non_exhaustive] — catch future
                // variants so newer servers don't take the client down.
                SpawnResult::Err(other) => {
                    tracing::warn!(
                        request_id,
                        error = ?other,
                        "TerminalSpawned: unknown spawn error variant",
                    );
                    let _ = actions::write_bell(out);
                    Ok(FrameOutcome::default())
                }
                // SpawnResult is also #[non_exhaustive].
                _ => {
                    tracing::warn!(request_id, "TerminalSpawned: unknown SpawnResult variant");
                    Ok(FrameOutcome::default())
                }
            }
        }
        // phux-4li.12: a Terminal closed. Fold it out of the layout if
        // it's a known leaf, drop its PaneSlot regardless. If we
        // initiated the kill (or it died on us spontaneously), the
        // server still broadcasts this so every attached client folds
        // in lockstep.
        FrameKind::TerminalClosed {
            terminal_id,
            exit_status,
        } => {
            tracing::info!(
                terminal = ?terminal_id,
                exit_status = ?exit_status,
                "TerminalClosed",
            );
            // phux-i0e8.2.2: was this close one WE asked for (kill-pane /
            // kill-window)? Drain the marker unconditionally — every close
            // consumes at most one expectation, whatever its exit status —
            // so a later spontaneous death of a re-used id still notifies.
            let expected = expected_closes.remove(&terminal_id);
            // Always drop the slot — even for unknown leaves (could be
            // a spawn-failure cleanup race or a stale id from before
            // an attach).
            panes.remove(&terminal_id);
            // Find the window holding this leaf (panes can live in any
            // window, not just the active one) and fold it out there.
            let owner = workspace.windows.iter().position(|w| {
                w.state
                    .tree
                    .as_ref()
                    .map(layout::leaves)
                    .unwrap_or_default()
                    .contains(&terminal_id)
            });
            let Some(idx) = owner else {
                return Ok(FrameOutcome::default());
            };
            match apply_terminal_closed(&workspace.windows[idx].state, &terminal_id) {
                Ok(new_state) => {
                    workspace.windows[idx].state = new_state;
                    // The fold may have emptied the window; drop any such
                    // windows and keep `active` valid.
                    workspace.prune_empty_windows();
                    // phux-4r1: consumer-owned detach policy (ADR-0015 L1).
                    // The server reports the fact (TERMINAL_CLOSED) and stops
                    // there; deciding whether *this* client detaches is the
                    // TUI's call. When the last pane closed there is nothing
                    // left to render or to route input to, so detach. For
                    // v0.1 single-pane this is behaviorally identical to the
                    // old server-baked "EOF ⇒ DETACHED" (the seed pane closes
                    // ⇒ client exits), but now multi-Terminal-ready: closing
                    // one of several panes folds it out and keeps the attach
                    // alive.
                    if workspace.windows.is_empty() {
                        tracing::info!("TerminalClosed folded the last pane; detaching");
                        return Ok(FrameOutcome {
                            exit: true,
                            // phux-i0e8.2.2: carry the dead pane's status up
                            // so the CLI can explain the exit on the cooked
                            // terminal — an OOM-killed shell must not look
                            // like phux crashed.
                            exit_reason: Some(AttachEnd::LastPaneClosed { exit_status }),
                            ..FrameOutcome::default()
                        });
                    }
                    // Re-anchor `focused_pane` onto the (possibly new)
                    // active window's focus. `apply_terminal_closed` sets
                    // a surviving window's focus to the first DFS leaf;
                    // a pruned active window hands focus to its successor.
                    *focused_pane = workspace.active_window().and_then(|ls| ls.focus.clone());
                    // phux-i0e8.2.2: survivors get a transient Warn notice
                    // naming the dead pane and its exit shape. Silent for a
                    // clean exit 0 (the user typed `exit`; nothing is wrong)
                    // and for a close this client itself requested.
                    let notices = if expected || exit_status == Some(0) {
                        Vec::new()
                    } else {
                        vec![Notice::warn(format!(
                            "{}: {}",
                            pane_label(&terminal_id),
                            describe_exit(exit_status),
                        ))]
                    };
                    Ok(FrameOutcome {
                        layout_replaced: true,
                        emit_set_metadata: true,
                        // phux-tnh: the survivor's Rect grew; tell the
                        // server so its PTY winsize grows too.
                        reflow_panes: true,
                        notices,
                        ..FrameOutcome::default()
                    })
                }
                Err(err) => {
                    // The leaf vanished from the tree between the lookup
                    // and the fold (a race), or the window emptied. Drop
                    // quietly — the slot is already gone.
                    tracing::debug!(
                        error = %err,
                        terminal = ?terminal_id,
                        "apply_terminal_closed: layout fold failed",
                    );
                    Ok(FrameOutcome::default())
                }
            }
        }
        // ADR-0033: a pushed agent event. We subscribed to the stream at
        // attach (SUBSCRIBE_EVENTS) for the supervisory `TerminalControl`
        // broadcast; fold its lifecycle + lease-holder into the pane's slot so
        // the next paint renders the "FROZEN" / "wheel" badge. The ADR-0035
        // `Asked` event is folded into the same per-pane state below. Other
        // event kinds (dirty/idle/bell/...) are not consumed by the
        // interactive TUI.
        FrameKind::Event {
            terminal: Some(terminal),
            event:
                AgentEvent::TerminalControl {
                    lifecycle,
                    input_holder,
                    ..
                },
        } => {
            if let Some(slot) = panes.get_mut(&terminal) {
                // phux-i0e8.2.1: a holder TRANSITION on the FOCUSED pane also
                // raises a transient status-bar notice — the badge shows the
                // steady state; the notice calls out the moment the wheel
                // moved. The first TerminalControl a slot ever sees is the
                // attach-time initial state (the server re-states the lease on
                // subscribe), not a transition, so it stays silent.
                let initial_state = !slot.control_seen;
                slot.control_seen = true;
                let holder_changed = slot.input_holder != input_holder;
                slot.lifecycle = lifecycle;
                slot.input_holder = input_holder;
                let notices =
                    if holder_changed && !initial_state && focused_pane.as_ref() == Some(&terminal)
                    {
                        vec![Notice::info(input_authority_notice(input_holder))]
                    } else {
                        Vec::new()
                    };
                Ok(FrameOutcome {
                    chrome_dirty: true,
                    notices,
                    ..FrameOutcome::default()
                })
            } else {
                // A control event for a pane we have no slot for yet (it can
                // precede the first snapshot). Harmless to drop — the lease is
                // server-authoritative and the next event re-states it.
                Ok(FrameOutcome::default())
            }
        }
        // phux-foz.1 / ADR-0035: an agent in `terminal` is waiting on a human
        // answer. Mirror the `TerminalControl` fold above: raise the pane's
        // attention flag so the next chrome paint renders the window-tab `!`
        // marker and the status-bar `[ ASK ]` hint. The flag clears when the
        // user sends key/paste input to the pane (see
        // `driver::clear_attention_on_input`); a repeated `Asked` while
        // already flagged changes nothing, so no repaint is requested for it.
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::Asked { .. },
        } => {
            if let Some(slot) = panes.get_mut(&terminal) {
                if slot.attention {
                    Ok(FrameOutcome::default())
                } else {
                    slot.attention = true;
                    Ok(FrameOutcome {
                        chrome_dirty: true,
                        ..FrameOutcome::default()
                    })
                }
            } else {
                // An Asked for a pane we have no slot for yet (it can precede
                // the first snapshot). Dropped like an early TerminalControl;
                // the ADR-0036 detector coalesces repeated markers, so the
                // next re-ask re-raises it once the slot exists.
                Ok(FrameOutcome::default())
            }
        }
        // phux-foz.4: the pane's shell changed directory (kernel-observed,
        // announced at prompt boundaries / output settle). Fold it into the
        // slot so the status-bar `cwd` widget tracks the focused pane;
        // `chrome_dirty` only when the value actually moved, and the chrome
        // refresh itself no-ops for an unfocused pane's change.
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::CwdChanged { cwd },
        } => {
            match panes.get_mut(&terminal) {
                Some(slot) if slot.cwd.as_deref() != Some(cwd.as_str()) => {
                    slot.cwd = Some(cwd);
                    Ok(FrameOutcome {
                        chrome_dirty: true,
                        ..FrameOutcome::default()
                    })
                }
                // Unchanged value, or a pane we have no slot for yet — the
                // next cwd_changed (or the ATTACHED seed) covers it.
                _ => Ok(FrameOutcome::default()),
            }
        }
        // phux-foz.4: a command finished in the pane; record its OSC-133
        // exit code for the status-bar `exit` widget. `None` is recorded
        // too — "the last command reported no code" honestly blanks the
        // widget rather than pinning a stale code.
        FrameKind::Event {
            terminal: Some(terminal),
            event: AgentEvent::CommandFinished { exit_code },
        } => match panes.get_mut(&terminal) {
            Some(slot) if slot.last_exit != exit_code => {
                slot.last_exit = exit_code;
                Ok(FrameOutcome {
                    chrome_dirty: true,
                    ..FrameOutcome::default()
                })
            }
            _ => Ok(FrameOutcome::default()),
        },
        // phux-i0e8.2.1 (second consumer, closing phux-i0e8.2's otherwise
        // orphaned lifecycle event): a spontaneous, uncorrelated
        // `ERROR { SATELLITE_UNREACHABLE }` is the hub announcing a
        // degraded-federation transition — part of the fleet just became
        // invisible. Surface it as a Warn notice through the same slot
        // instead of dropping it in the catch-all below. Correlated
        // satellite errors (`request_id: Some`) stay on their
        // request/reply paths; `phux status`'s degradation line remains
        // the CLI view of the same state, not the TUI representation.
        FrameKind::Error {
            request_id: None,
            code: ErrorCode::SatelliteUnreachable,
            message,
        } => {
            tracing::warn!(message = %message, "federation degraded (satellite unreachable)");
            Ok(FrameOutcome {
                notices: vec![Notice::warn(format!("federation degraded: {message}"))],
                ..FrameOutcome::default()
            })
        }
        other => Err(AttachError::Protocol(format!(
            "frame is not valid from a server in the attached phase: {other:?}",
        ))),
    }
}

/// phux-4li.15: apply a `TERMINAL_SPAWNED` reply for a parked
/// `new-window` action. On success it appends a window seeded on the
/// freshly spawned pane (making it active), seeds the pane's slot, and
/// re-anchors `focused_pane`. The follow-up flags mirror the split path:
/// `layout_replaced` triggers a full repaint, `emit_set_metadata`
/// broadcasts the new workspace to siblings, and `reflow_panes` sizes the
/// new full-window pane.
fn handle_window_spawned<W: super::RenderSink>(
    out: &mut W,
    workspace: &mut Workspace,
    focused_pane: &mut Option<TerminalId>,
    panes: &mut HashMap<TerminalId, PaneSlot>,
    pending: &PendingWindow,
    result: SpawnResult,
) -> Result<FrameOutcome, AttachError> {
    match result {
        SpawnResult::Ok(new_id) => {
            workspace.add_window(pending.name.clone(), new_id.clone());
            if let std::collections::hash_map::Entry::Vacant(v) = panes.entry(new_id) {
                v.insert(PaneSlot::new()?);
            }
            *focused_pane = workspace.active_window().and_then(|ls| ls.focus.clone());
            Ok(FrameOutcome {
                layout_replaced: true,
                emit_set_metadata: true,
                reflow_panes: true,
                ..FrameOutcome::default()
            })
        }
        SpawnResult::Err(err) => {
            tracing::warn!(error = ?err, "new-window: server-side spawn failed");
            let _ = actions::write_bell(out);
            Ok(FrameOutcome::default())
        }
        // SpawnResult is #[non_exhaustive] — tolerate future variants.
        _ => {
            tracing::warn!("new-window: unknown SpawnResult variant");
            Ok(FrameOutcome::default())
        }
    }
}

/// phux-17u: resolve the focused session's display name from an
/// `ATTACHED` snapshot for the status-bar `session-name` widget.
///
/// The snapshot carries `sessions: Vec<SessionInfo>` plus a
/// `focused_session` id; the name is the `SessionInfo` whose `id`
/// matches. Returns the empty string when the focused session isn't in
/// the list — which shouldn't happen (the focused session is always one
/// of the snapshot's own sessions), but an empty widget is a safer
/// degradation than a panic.
fn focused_session_name(snapshot: &phux_protocol::wire::info::SessionSnapshot) -> String {
    snapshot
        .sessions
        .iter()
        .find(|s| s.id == snapshot.focused_session)
        .map(|s| s.name.clone())
        .unwrap_or_default()
}

/// Decide whether `(scope, key)` matches a layout-coordination key ADR-0019
/// reserves (`phux.tui.layout/v1[/<session>]`, scoped to the default Group).
///
/// Per-session keying (phux-jy4t) means the key carries a session suffix; a
/// client only ever receives broadcasts for the key it subscribed to (its own
/// session), so matching the family is sufficient.
fn is_layout_key(scope: &Scope, key: &str) -> bool {
    matches!(scope, Scope::Group(id) if *id == DEFAULT_GROUP_ID)
        && super::driver::is_layout_key_string(key)
}

/// Adopt a decoded workspace's topology without adopting its sender's focus.
///
/// Focus and the active-window index are client-local (ADR-0019 decision 6,
/// reaffirmed by ADR-0049). For each incoming window, preserve the local focus
/// at the same index when that terminal remains a leaf; otherwise choose the
/// first depth-first leaf. Preserve the local active index when the new window
/// count permits it, otherwise clamp deterministically.
///
/// The foreign-session guard remains workspace-scoped. An incoming non-empty
/// tree is foreign only when none of its leaves belongs to the current local
/// workspace or pane-slot set. Checking all known panes, rather than only the
/// currently focused pane, lets a sibling legitimately remove that focused leaf
/// without making the surviving topology look foreign.
fn unknown_layout_leaves(
    incoming: &Workspace,
    panes: &HashMap<TerminalId, PaneSlot>,
) -> Vec<TerminalId> {
    incoming
        .windows
        .iter()
        .filter_map(|window| window.state.tree.as_ref())
        .flat_map(crate::layout::leaves)
        .filter(|terminal| !panes.contains_key(terminal))
        .collect()
}

#[cfg(test)]
fn reconcile_loaded_workspace(
    incoming: Workspace,
    local: &Workspace,
    bootstrap_focus: Option<&TerminalId>,
    panes: &HashMap<TerminalId, PaneSlot>,
) -> Workspace {
    reconcile_loaded_workspace_checked(incoming, local, bootstrap_focus, panes).0
}

/// Reconcile topology and report whether the foreign-session guard accepted it.
/// Callers must only discover/attach new leaves when `accepted` is true.
fn reconcile_loaded_workspace_checked(
    mut incoming: Workspace,
    local: &Workspace,
    bootstrap_focus: Option<&TerminalId>,
    panes: &HashMap<TerminalId, PaneSlot>,
) -> (Workspace, bool) {
    let incoming_leaves: Vec<TerminalId> = incoming
        .windows
        .iter()
        .flat_map(|w| {
            w.state
                .tree
                .as_ref()
                .map(crate::layout::leaves)
                .unwrap_or_default()
        })
        .collect();
    let local_leaves: Vec<TerminalId> = local
        .windows
        .iter()
        .flat_map(|w| {
            w.state
                .tree
                .as_ref()
                .map(crate::layout::leaves)
                .unwrap_or_default()
        })
        .collect();
    let has_session_evidence = !local_leaves.is_empty() || !panes.is_empty();
    let belongs_to_session = incoming_leaves
        .iter()
        .any(|leaf| local_leaves.contains(leaf) || panes.contains_key(leaf));
    if !incoming_leaves.is_empty()
        && has_session_evidence
        && !belongs_to_session
        && let Some(focus) = bootstrap_focus
    {
        return (Workspace::single(focus.clone()), false);
    }

    for (index, window) in incoming.windows.iter_mut().enumerate() {
        let local_focus = local
            .windows
            .get(index)
            .and_then(|local_window| local_window.state.focus.as_ref());
        reconcile_loaded_layout(&mut window.state, local_focus);
    }
    incoming.active = if incoming.windows.is_empty() {
        0
    } else {
        local.active.min(incoming.windows.len() - 1)
    };
    (incoming, true)
}

/// Preserve a valid local focus while adopting `state`'s tree topology.
///
/// The focus decoded from metadata is deliberately ignored. If this client has
/// no focus for the window, or its focused leaf disappeared, the first leaf in
/// depth-first order is the deterministic ADR-0019 fallback.
fn reconcile_loaded_layout(state: &mut LayoutState, local_focus: Option<&TerminalId>) {
    let tree_leaves = state
        .tree
        .as_ref()
        .map(crate::layout::leaves)
        .unwrap_or_default();
    state.focus = local_focus
        .filter(|focus| tree_leaves.contains(focus))
        .cloned()
        .or_else(|| tree_leaves.into_iter().next());
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::{
        AgentMetaIndex, FrameOutcome, handle_server_frame as handle_server_frame_with_kernel,
        route_engine_frame,
    };
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    use phux_protocol::ids::{ClientId, SessionId, TerminalId, WindowId};
    use phux_protocol::wire::frame::FrameKind;
    use phux_protocol::wire::info::{LayoutNode, SessionSnapshot, SplitDir, TerminalInfo};

    use crate::attach::driver::{AttachEnd, AttachError, PaneSlot};
    use crate::layout::{LayoutState, Workspace};
    use crate::predict::{Overlay, PredictionState, PredictiveConfig};

    static TRACE_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Strip CSI escape sequences (`ESC [ ... final`) from a captured
    /// render stream, leaving only the printable glyphs, so a content
    /// assertion can't be satisfied by control bytes that happen to share
    /// a letter (e.g. the `h`/`l` in `\x1b[?25h` / `\x1b[?25l`).
    fn strip_csi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' && chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                // Consume params/intermediates up to the final byte (@..~).
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            } else if c != '\x1b' {
                out.push(c);
            }
        }
        out
    }

    fn tid(id: u32) -> TerminalId {
        TerminalId::local(id)
    }
    fn stream() -> phux_protocol::StreamId {
        phux_protocol::StreamId::new(1).expect("stream")
    }

    fn bootstrap() -> phux_protocol::BootstrapId {
        phux_protocol::BootstrapId::new(1).expect("bootstrap")
    }

    fn begin_frame(terminal_id: &TerminalId) -> FrameKind {
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        }
    }

    fn ready_frame(terminal_id: &TerminalId) -> FrameKind {
        FrameKind::BootstrapReady {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            history_cursor: None,
        }
    }

    #[test]
    fn engine_damage_obeys_attach_barrier_and_ready_publication() {
        let terminal_id = tid(90);
        let mut kernel = phux_client_core::session::SessionKernel::new(
            phux_client_core::engine::ghostty::GhosttyAdapter::new(
                phux_protocol::BootstrapLimits::default(),
            ),
            phux_protocol::BootstrapProfile::SynthesizedVtRaw,
        );
        let mut effects = phux_client_core::session::EffectBuffer::new();
        kernel
            .update(
                phux_client_core::session::KernelInput::AttachStarted {
                    attach_id: 7,
                    terminals: std::slice::from_ref(&terminal_id),
                },
                &mut effects,
            )
            .expect("attach");
        assert!(
            route_engine_frame(&begin_frame(&terminal_id), &mut kernel, &mut effects)
                .damaged
                .is_empty()
        );
        assert!(
            route_engine_frame(
                &FrameKind::BootstrapChunk {
                    terminal_id: terminal_id.clone(),
                    stream_id: stream(),
                    bootstrap_id: bootstrap(),
                    chunk_seq: 0,
                    payload: bytes::Bytes::from_static(b"seed"),
                },
                &mut kernel,
                &mut effects,
            )
            .damaged
            .is_empty()
        );
        assert!(
            route_engine_frame(&ready_frame(&terminal_id), &mut kernel, &mut effects)
                .damaged
                .is_empty(),
            "publication damage stays behind ATTACH_READY"
        );
        assert!(
            route_engine_frame(
                &FrameKind::TerminalOutput {
                    terminal_id: terminal_id.clone(),
                    stream_id: stream(),
                    bootstrap_id: bootstrap(),
                    seq: 1,
                    bytes: bytes::Bytes::from_static(b"before-barrier"),
                },
                &mut kernel,
                &mut effects,
            )
            .damaged
            .is_empty(),
            "pre-barrier live output must not paint directly"
        );
        let released = route_engine_frame(
            &FrameKind::AttachReady { attach_id: 7 },
            &mut kernel,
            &mut effects,
        );
        assert!(released.damaged(&terminal_id));
        let live = route_engine_frame(
            &FrameKind::TerminalOutput {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                seq: 2,
                bytes: bytes::Bytes::from_static(b"after-barrier"),
            },
            &mut kernel,
            &mut effects,
        );
        assert!(live.damaged(&terminal_id));
        let reply = route_engine_frame(
            &FrameKind::TerminalOutput {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                seq: 3,
                bytes: bytes::Bytes::from_static(b"\x1b[5n"),
            },
            &mut kernel,
            &mut effects,
        );
        assert_eq!(reply.pty_writes, vec![(terminal_id, b"\x1b[0n".to_vec())]);
    }

    #[test]
    fn ready_history_cursor_is_preserved_into_kernel_request() {
        let terminal_id = tid(91);
        let mut kernel = phux_client_core::session::SessionKernel::new(
            phux_client_core::engine::ghostty::GhosttyAdapter::new(
                phux_protocol::BootstrapLimits::default(),
            ),
            phux_protocol::BootstrapProfile::SynthesizedVtRaw,
        );
        let mut effects = phux_client_core::session::EffectBuffer::new();
        kernel
            .update(
                phux_client_core::session::KernelInput::AttachStarted {
                    attach_id: 8,
                    terminals: std::slice::from_ref(&terminal_id),
                },
                &mut effects,
            )
            .expect("attach");
        route_engine_frame(&begin_frame(&terminal_id), &mut kernel, &mut effects);
        route_engine_frame(
            &FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                chunk_seq: 0,
                payload: bytes::Bytes::from_static(b"seed"),
            },
            &mut kernel,
            &mut effects,
        );
        let routed = route_engine_frame(
            &FrameKind::BootstrapReady {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                history_cursor: Some(bytes::Bytes::from_static(b"opaque-cursor")),
            },
            &mut kernel,
            &mut effects,
        );
        assert_eq!(
            routed.history_request,
            Some((
                terminal_id.clone(),
                stream(),
                bootstrap(),
                bytes::Bytes::from_static(b"opaque-cursor"),
                1024 * 1024,
                1024,
            ))
        );
        let rejected = route_engine_frame(
            &FrameKind::HistoryRejected {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                cursor: bytes::Bytes::from_static(b"opaque-cursor"),
                reason: phux_protocol::wire::frame::HistoryRejectionReason::TooSmall,
                required_bytes: 1024 * 1024,
                required_rows: 2048,
            },
            &mut kernel,
            &mut effects,
        );
        assert!(!rejected.resync_required);
        assert!(
            rejected.history_request.is_none(),
            "requirements above the negotiated row cap stay idle"
        );
        let tombstoned = route_engine_frame(
            &FrameKind::HistoryTombstone {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                cursor: bytes::Bytes::from_static(b"opaque-cursor"),
                reason: phux_protocol::wire::frame::HistoryTombstoneReason::Pruned,
            },
            &mut kernel,
            &mut effects,
        );
        assert!(!tombstoned.resync_required);
        assert!(
            kernel.published_engine(&terminal_id).is_some(),
            "history-only invalidation preserves the live replica"
        );
    }

    #[test]
    fn off_window_ready_waits_for_every_snapshot_pane_and_attach_ready() {
        let focused = tid(94);
        let off_window = tid(95);
        let focused_window = WindowId::new(70);
        let other_window = WindowId::new(71);
        let snapshot = SessionSnapshot::new(SessionId::new(72), focused_window, focused.clone())
            .with_panes(vec![
                TerminalInfo::new(focused.clone(), focused_window, 80, 24),
                TerminalInfo::new(off_window.clone(), other_window, 80, 24),
            ]);
        let mut kernel = phux_client_core::session::SessionKernel::new(
            phux_client_core::engine::ghostty::GhosttyAdapter::new(
                phux_protocol::BootstrapLimits::default(),
            ),
            phux_protocol::BootstrapProfile::SynthesizedVtRaw,
        );
        let mut effects = phux_client_core::session::EffectBuffer::new();
        let attached = route_engine_frame(
            &FrameKind::Attached {
                attach_id: 9,
                snapshot,
                initial_client_id: ClientId::new(1),
            },
            &mut kernel,
            &mut effects,
        );
        assert!(attached.damaged.is_empty());

        for terminal_id in [&off_window, &focused] {
            assert!(
                route_engine_frame(&begin_frame(terminal_id), &mut kernel, &mut effects)
                    .damaged
                    .is_empty()
            );
            assert!(
                route_engine_frame(
                    &FrameKind::BootstrapChunk {
                        terminal_id: terminal_id.clone(),
                        stream_id: stream(),
                        bootstrap_id: bootstrap(),
                        chunk_seq: 0,
                        payload: bytes::Bytes::from_static(b"seed"),
                    },
                    &mut kernel,
                    &mut effects,
                )
                .damaged
                .is_empty()
            );
            assert!(
                route_engine_frame(&ready_frame(terminal_id), &mut kernel, &mut effects)
                    .damaged
                    .is_empty(),
                "neither off-window nor focused READY may escape the aggregate barrier"
            );
        }
        let released = route_engine_frame(
            &FrameKind::AttachReady { attach_id: 9 },
            &mut kernel,
            &mut effects,
        );
        assert!(released.damaged(&focused));
        assert!(released.damaged(&off_window));
    }

    #[test]
    fn bootstrap_ready_surfaces_publication_damage_without_attach_barrier() {
        let terminal_id = tid(91);
        let mut kernel = phux_client_core::session::SessionKernel::new(
            phux_client_core::engine::ghostty::GhosttyAdapter::new(
                phux_protocol::BootstrapLimits::default(),
            ),
            phux_protocol::BootstrapProfile::SynthesizedVtRaw,
        );
        let mut effects = phux_client_core::session::EffectBuffer::new();
        route_engine_frame(&begin_frame(&terminal_id), &mut kernel, &mut effects);
        route_engine_frame(
            &FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                chunk_seq: 0,
                payload: bytes::Bytes::from_static(b"seed"),
            },
            &mut kernel,
            &mut effects,
        );
        let ready = route_engine_frame(&ready_frame(&terminal_id), &mut kernel, &mut effects);
        assert!(ready.damaged(&terminal_id));
    }
    fn dispatch_engine_frame(
        kernel: &mut phux_client_core::session::SessionKernel<
            phux_client_core::engine::ghostty::GhosttyAdapter,
        >,
        effects: &mut phux_client_core::session::EffectBuffer,
        panes: &mut HashMap<TerminalId, PaneSlot>,
        frame: FrameKind,
    ) -> FrameOutcome {
        let mut out = Vec::new();
        let mut workspace = Workspace::default();
        let mut focused_pane = None;
        let mut zoomed = None;
        let mut session_name = String::new();
        let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut expected_closes = HashSet::new();
        let mut agent_meta = AgentMetaIndex::default();
        handle_server_frame_with_kernel(
            kernel,
            effects,
            &mut out,
            frame,
            panes,
            &mut workspace,
            &mut focused_pane,
            &mut zoomed,
            &mut session_name,
            None,
            None,
            (80, 24),
            &mut predict,
            &overlay,
            None,
            &mut pending_splits,
            &mut pending_windows,
            &mut expected_closes,
            &mut agent_meta,
            false,
            true,
        )
        .expect("engine frame")
    }

    #[test]
    fn pre_barrier_output_refreshes_title_cache_before_attach_ready() {
        let ready_terminal = tid(92);
        let pending_terminal = tid(93);
        let mut kernel = phux_client_core::session::SessionKernel::new(
            phux_client_core::engine::ghostty::GhosttyAdapter::new(
                phux_protocol::BootstrapLimits::default(),
            ),
            phux_protocol::BootstrapProfile::SynthesizedVtRaw,
        );
        let mut effects = phux_client_core::session::EffectBuffer::new();
        let mut panes = HashMap::new();
        kernel
            .update(
                phux_client_core::session::KernelInput::AttachStarted {
                    attach_id: 8,
                    terminals: &[ready_terminal.clone(), pending_terminal.clone()],
                },
                &mut effects,
            )
            .expect("attach");

        dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            begin_frame(&ready_terminal),
        );
        dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            FrameKind::BootstrapChunk {
                terminal_id: ready_terminal.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                chunk_seq: 0,
                payload: bytes::Bytes::from_static(b"\x1b]2;shell\x07"),
            },
        );
        dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            ready_frame(&ready_terminal),
        );
        assert_eq!(panes[&ready_terminal].last_title, "shell");
        dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            begin_frame(&pending_terminal),
        );

        let pre_barrier = dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            FrameKind::TerminalOutput {
                terminal_id: ready_terminal.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                seq: 1,
                bytes: bytes::Bytes::from_static(b"\x1b]2;vim\x07"),
            },
        );
        assert!(!pre_barrier.chrome_dirty);
        assert_eq!(
            panes[&ready_terminal].last_title, "vim",
            "damage suppression must not suppress engine-derived metadata refresh"
        );

        dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            FrameKind::BootstrapChunk {
                terminal_id: pending_terminal.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                chunk_seq: 0,
                payload: bytes::Bytes::from_static(b"pending"),
            },
        );
        dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            ready_frame(&pending_terminal),
        );
        let released = dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            FrameKind::AttachReady { attach_id: 8 },
        );
        assert!(released.layout_replaced);
        assert_eq!(panes[&ready_terminal].last_title, "vim");
    }
    #[test]
    fn malformed_history_requests_resync_and_replacement_publishes_atomically() {
        let terminal_id = tid(96);
        let replacement = phux_protocol::BootstrapId::new(2).expect("replacement");
        let mut kernel = phux_client_core::session::SessionKernel::new(
            phux_client_core::engine::ghostty::GhosttyAdapter::new(
                phux_protocol::BootstrapLimits::default(),
            ),
            phux_protocol::BootstrapProfile::SynthesizedVtRaw,
        );
        let mut effects = phux_client_core::session::EffectBuffer::new();
        let mut panes = HashMap::new();
        dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            begin_frame(&terminal_id),
        );
        dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                chunk_seq: 0,
                payload: bytes::Bytes::from_static(b"\x1b]2;old\x07"),
            },
        );
        dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            ready_frame(&terminal_id),
        );

        let rejected = dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            FrameKind::HistoryPage {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                rows: 1,
                page_seq: 1,
                cursor: bytes::Bytes::from_static(b"cursor"),
                next_cursor: None,
                payload: bytes::Bytes::from_static(b"malformed-history"),
            },
        );
        assert!(rejected.resync_required);
        assert_eq!(
            kernel
                .published_engine(&terminal_id)
                .unwrap()
                .terminal()
                .unwrap()
                .title()
                .unwrap(),
            "old"
        );
        let stale = dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            FrameKind::HistoryPage {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                rows: 1,
                page_seq: 1,
                cursor: bytes::Bytes::from_static(b"stale"),
                next_cursor: None,
                payload: bytes::Bytes::from_static(b"queued-stale-page"),
            },
        );
        assert!(!stale.resync_required);

        kernel
            .update(
                phux_client_core::session::KernelInput::AttachStarted {
                    attach_id: 10,
                    terminals: std::slice::from_ref(&terminal_id),
                },
                &mut effects,
            )
            .expect("replacement attach");
        dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: replacement,
                profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 80,
                rows: 24,
                base_seq: 0,
            },
        );
        dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: replacement,
                chunk_seq: 0,
                payload: bytes::Bytes::from_static(b"\x1b]2;new\x07"),
            },
        );
        assert_eq!(
            kernel
                .published_engine(&terminal_id)
                .unwrap()
                .terminal()
                .unwrap()
                .title()
                .unwrap(),
            "old",
            "replacement remains staged until READY"
        );
        let replacement_ready = dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            FrameKind::BootstrapReady {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: replacement,
                history_cursor: None,
            },
        );
        assert!(!replacement_ready.layout_replaced);
        assert!(!replacement_ready.chrome_dirty);
        assert_eq!(panes[&terminal_id].last_title, "new");
        let released = dispatch_engine_frame(
            &mut kernel,
            &mut effects,
            &mut panes,
            FrameKind::AttachReady { attach_id: 10 },
        );
        assert!(released.layout_replaced);
        assert_eq!(panes[&terminal_id].last_title, "new");
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_server_frame<W: crate::attach::RenderSink>(
        out: &mut W,
        frame: FrameKind,
        panes: &mut HashMap<TerminalId, PaneSlot>,
        workspace: &mut Workspace,
        focused_pane: &mut Option<TerminalId>,
        zoomed: &mut Option<TerminalId>,
        session_name: &mut String,
        status_bar: Option<&mut crate::render::chrome::status_bar::StatusBarPainter>,
        sidebar: Option<crate::attach::paint::SidebarReservation>,
        viewport_dims: (u16, u16),
        predict: &mut PredictionState,
        overlay: &Overlay,
        pending_layout_request: Option<u32>,
        pending_splits: &mut HashMap<u32, crate::attach::actions::PendingSplit>,
        pending_windows: &mut HashMap<u32, crate::attach::actions::PendingWindow>,
        expected_closes: &mut HashSet<TerminalId>,
        agent_meta: &mut AgentMetaIndex,
        overlay_active: bool,
        defer_paint: bool,
    ) -> Result<FrameOutcome, AttachError> {
        let mut kernel = phux_client_core::session::SessionKernel::new(
            phux_client_core::engine::ghostty::GhosttyAdapter::new(
                phux_protocol::BootstrapLimits::default(),
            ),
            phux_protocol::BootstrapProfile::SynthesizedVtRaw,
        );
        let mut effects = phux_client_core::session::EffectBuffer::new();
        handle_server_frame_with_kernel(
            &mut kernel,
            &mut effects,
            out,
            frame,
            panes,
            workspace,
            focused_pane,
            zoomed,
            session_name,
            status_bar,
            sidebar,
            viewport_dims,
            predict,
            overlay,
            pending_layout_request,
            pending_splits,
            pending_windows,
            expected_closes,
            agent_meta,
            overlay_active,
            defer_paint,
        )
    }

    fn split2(a: u32, b: u32, focus: u32) -> LayoutState {
        LayoutState {
            tree: Some(LayoutNode::Split {
                dir: SplitDir::Horizontal,
                ratio: 0.5,
                left: Box::new(LayoutNode::Leaf(tid(a))),
                right: Box::new(LayoutNode::Leaf(tid(b))),
            }),
            focus: Some(tid(focus)),
        }
    }

    /// A single-window workspace wrapping `state`, for the reconcile tests.
    fn ws1(state: LayoutState) -> Workspace {
        Workspace {
            windows: vec![crate::layout::WindowState {
                name: "1".to_owned(),
                state,
            }],
            active: 0,
        }
    }

    /// Leaves of a workspace's window at `idx`.
    fn window_leaves(ws: &Workspace, idx: usize) -> Vec<TerminalId> {
        ws.windows[idx]
            .state
            .tree
            .as_ref()
            .map(crate::layout::leaves)
            .unwrap_or_default()
    }

    /// phux-jy4t: a freshly created session reads the group-shared layout
    /// metadata, which holds a DIFFERENT session's tree. When this session's
    /// real ATTACHED pane is not a leaf of ANY window, the whole loaded
    /// workspace is foreign and must be discarded for a clean single pane — not
    /// rendered as the old layout with dead/empty panes.
    #[test]
    fn reconcile_discards_a_foreign_session_layout() {
        let foreign = ws1(split2(1, 2, 1)); // leaves {1, 2}, from another session
        let local = Workspace::single(tid(9));
        let out =
            super::reconcile_loaded_workspace(foreign, &local, Some(&tid(9)), &HashMap::new());
        assert_eq!(out.windows.len(), 1);
        assert_eq!(
            window_leaves(&out, 0),
            vec![tid(9)],
            "foreign layout discarded → clean single pane of the real terminal"
        );
        assert_eq!(out.windows[0].state.focus, Some(tid(9)));
    }

    #[test]
    fn reconcile_keeps_a_layout_that_contains_the_session_pane() {
        // Legitimate re-attach: the session's focused pane IS a leaf, so the
        // multi-pane tree is preserved (not discarded).
        let own = ws1(split2(1, 2, 1));
        let local = Workspace::single(tid(1));
        let out = super::reconcile_loaded_workspace(own, &local, Some(&tid(1)), &HashMap::new());
        let leaves = window_leaves(&out, 0);
        assert!(
            leaves.contains(&tid(1)) && leaves.contains(&tid(2)),
            "the session's own layout must be kept: {leaves:?}"
        );
    }

    #[test]
    fn reconcile_without_bootstrap_focus_keeps_the_tree() {
        // No ATTACHED focus to validate against ⇒ don't discard.
        let tree = ws1(split2(1, 2, 1));
        let out =
            super::reconcile_loaded_workspace(tree, &Workspace::default(), None, &HashMap::new());
        assert_eq!(
            window_leaves(&out, 0).len(),
            2,
            "no focus to validate ⇒ tree preserved"
        );
    }

    /// Regression: a multi-window workspace must NOT alias its non-active
    /// windows onto the focused pane. The focused pane is a leaf of window 0
    /// only; window 1 references a different terminal and must keep it (the
    /// "open vim in one window, it shows in the other" bug, where the
    /// per-window foreign-discard rewrote every non-active window to
    /// `single(focus)`).
    #[test]
    fn reconcile_multi_window_does_not_alias_non_active_windows() {
        let ws = Workspace {
            windows: vec![
                crate::layout::WindowState {
                    name: "1".to_owned(),
                    state: LayoutState::single(tid(1)),
                },
                crate::layout::WindowState {
                    name: "2".to_owned(),
                    state: LayoutState::single(tid(2)),
                },
            ],
            active: 0,
        };
        // Focus is on window 0's pane (tid 1); window 1 (tid 2) is non-active.
        let local = ws.clone();
        let out = super::reconcile_loaded_workspace(ws, &local, Some(&tid(1)), &HashMap::new());
        assert_eq!(out.windows.len(), 2, "both windows survive");
        assert_eq!(window_leaves(&out, 0), vec![tid(1)]);
        assert_eq!(
            window_leaves(&out, 1),
            vec![tid(2)],
            "non-active window keeps its own terminal, not aliased onto the focus"
        );
    }

    /// Build a `panes` map with a warm [`PaneSlot`] per supplied id.
    fn panes_for(ids: &[&TerminalId]) -> HashMap<TerminalId, PaneSlot> {
        let mut panes = HashMap::new();
        for id in ids {
            panes.insert((*id).clone(), PaneSlot::new().expect("pane slot"));
        }
        panes
    }

    struct EngineFixture {
        kernel: super::super::driver::AttachKernel,
        effects: phux_client_core::session::EffectBuffer,
    }

    fn published_fixture(
        entries: &[(&TerminalId, u16, u16, &[u8])],
    ) -> (EngineFixture, HashMap<TerminalId, PaneSlot>) {
        let (kernel, effects, panes) =
            super::super::driver::published_test_state(entries);
        (EngineFixture { kernel, effects }, panes)
    }

    /// Drive any frame through the full attached-state dispatcher.
    fn try_drive_layout_frame(
        frame: FrameKind,
        pending_layout_request: Option<u32>,
        workspace: &mut Workspace,
        focused: &mut Option<TerminalId>,
        panes: &mut HashMap<TerminalId, PaneSlot>,
    ) -> Result<FrameOutcome, AttachError> {
        let mut out: Vec<u8> = Vec::new();
        let mut session_name = String::new();
        let mut zoomed: Option<TerminalId> = None;
        let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        handle_server_frame(
            &mut out,
            frame,
            panes,
            workspace,
            focused,
            &mut zoomed,
            &mut session_name,
            None,
            None,
            (80, 24),
            &mut predict,
            &overlay,
            pending_layout_request,
            &mut pending_splits,
            &mut pending_windows,
            &mut HashSet::new(),
            &mut AgentMetaIndex::default(),
            false,
            false,
        )
    }

    fn drive_layout_frame(
        frame: FrameKind,
        pending_layout_request: Option<u32>,
        workspace: &mut Workspace,
        focused: &mut Option<TerminalId>,
        panes: &mut HashMap<TerminalId, PaneSlot>,
    ) -> FrameOutcome {
        try_drive_layout_frame(frame, pending_layout_request, workspace, focused, panes)
            .expect("handle layout frame")
    }

    #[test]
    fn duplicate_hello_ok_is_fatal_in_attached_phase() {
        let pane = tid(1);
        let mut workspace = Workspace::single(pane.clone());
        let mut focused = Some(pane.clone());
        let mut panes = panes_for(&[&pane]);
        let error = try_drive_layout_frame(
            FrameKind::HelloOk {
                protocol_major: phux_protocol::PROTOCOL_VERSION.major,
                protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
                protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
                server_caps: phux_protocol::caps::ServerCapabilities::new(),
                server_id: Vec::new(),
                selected_profile: phux_protocol::caps::BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: phux_protocol::caps::BootstrapLimits::default(),
            },
            None,
            &mut workspace,
            &mut focused,
            &mut panes,
        )
        .expect_err("post-negotiation HELLO_OK must terminate the client");
        assert!(matches!(
            error,
            AttachError::Protocol(message) if message.contains("not valid from a server")
        ));
    }

    /// ADR-0049: a sibling's layout broadcast contributes topology only. Its
    /// serialized active window and per-window focuses cannot yank this client.
    #[test]
    fn metadata_changed_preserves_valid_local_window_and_pane_focus() {
        use phux_protocol::wire::frame::Scope;

        let mut local = Workspace {
            windows: vec![
                crate::layout::WindowState {
                    name: "local-one".to_owned(),
                    state: split2(1, 2, 2),
                },
                crate::layout::WindowState {
                    name: "local-two".to_owned(),
                    state: split2(3, 4, 4),
                },
            ],
            active: 1,
        };
        let mut sibling = local.clone();
        sibling.active = 0;
        sibling.windows[0].name = "shared-one".to_owned();
        sibling.windows[1].name = "shared-two".to_owned();
        sibling.windows[0].state.focus = Some(tid(1));
        sibling.windows[1].state.focus = Some(tid(3));
        if let Some(LayoutNode::Split { ratio, .. }) = sibling.windows[1].state.tree.as_mut() {
            *ratio = 0.7;
        }
        let bytes = sibling.encode_cbor().expect("encode sibling workspace");
        let mut focused = Some(tid(4));
        let mut panes = panes_for(&[&tid(1), &tid(2), &tid(3), &tid(4)]);

        let outcome = drive_layout_frame(
            FrameKind::MetadataChanged {
                scope: Scope::Group(super::DEFAULT_GROUP_ID),
                key: crate::attach::driver::layout_key(SessionId::new(1)),
                value: Some(bytes),
            },
            None,
            &mut local,
            &mut focused,
            &mut panes,
        );

        assert!(outcome.layout_replaced);
        assert_eq!(local.active, 1, "sender cannot change the local window");
        assert_eq!(local.windows[0].state.focus, Some(tid(2)));
        assert_eq!(local.windows[1].state.focus, Some(tid(4)));
        assert_eq!(focused, Some(tid(4)), "driver mirror stays client-local");
        assert_eq!(local.windows[0].name, "shared-one", "names are topology");
        assert!(matches!(
            local.windows[1].state.tree,
            Some(LayoutNode::Split { ratio, .. }) if (ratio - 0.7).abs() < f32::EPSILON
        ));
    }

    #[test]
    fn rejected_cross_session_layout_emits_no_attach_panes() {
        use phux_protocol::wire::frame::Scope;

        let mut local = Workspace::single(tid(9));
        let foreign = ws1(split2(1, 2, 1));
        let bytes = foreign.encode_cbor().expect("encode foreign workspace");
        let mut focused = Some(tid(9));
        let mut panes = panes_for(&[&tid(9)]);

        let outcome = drive_layout_frame(
            FrameKind::MetadataChanged {
                scope: Scope::Group(super::DEFAULT_GROUP_ID),
                key: crate::attach::driver::layout_key(SessionId::new(1)),
                value: Some(bytes),
            },
            None,
            &mut local,
            &mut focused,
            &mut panes,
        );

        assert!(outcome.attach_panes.is_empty());
        assert_eq!(window_leaves(&local, 0), vec![tid(9)]);
        assert_eq!(focused, Some(tid(9)));
    }

    #[test]
    fn metadata_changed_discovers_peer_added_leaf_without_moving_focus() {
        use phux_protocol::wire::frame::Scope;

        let mut local = ws1(split2(1, 2, 1));
        let mut sibling = local.clone();
        let tree = sibling.windows[0].state.tree.as_ref().unwrap();
        sibling.windows[0].state.tree = Some(
            crate::layout::split_at(tree, &tid(2), &tid(3), SplitDir::Vertical, 0.3)
                .expect("split peer tree"),
        );
        sibling.windows[0].state.focus = Some(tid(3));
        let bytes = sibling.encode_cbor().expect("encode sibling workspace");
        let mut focused = Some(tid(1));
        let mut panes = panes_for(&[&tid(1), &tid(2)]);

        let outcome = drive_layout_frame(
            FrameKind::MetadataChanged {
                scope: Scope::Group(super::DEFAULT_GROUP_ID),
                key: crate::attach::driver::layout_key(SessionId::new(1)),
                value: Some(bytes),
            },
            None,
            &mut local,
            &mut focused,
            &mut panes,
        );

        assert_eq!(outcome.attach_panes, vec![tid(3)]);
        assert_eq!(focused, Some(tid(1)));
        assert_eq!(local.windows[0].state.focus, Some(tid(1)));
        assert_eq!(window_leaves(&local, 0), vec![tid(1), tid(2), tid(3)]);
    }

    /// The initial persisted-layout reply uses the same topology-only merge as
    /// broadcasts: the attach bootstrap focus wins when it remains a leaf.
    #[test]
    fn metadata_value_preserves_valid_bootstrap_focus() {
        let mut local = Workspace::single(tid(2));
        let persisted = ws1(split2(1, 2, 1));
        let bytes = persisted.encode_cbor().expect("encode persisted workspace");
        let mut focused = Some(tid(2));
        let mut panes = panes_for(&[&tid(1), &tid(2)]);

        let outcome = drive_layout_frame(
            FrameKind::MetadataValue {
                request_id: 41,
                value: Some(bytes),
            },
            Some(41),
            &mut local,
            &mut focused,
            &mut panes,
        );

        assert!(outcome.layout_replaced);
        assert_eq!(window_leaves(&local, 0), vec![tid(1), tid(2)]);
        assert_eq!(local.windows[0].state.focus, Some(tid(2)));
        assert_eq!(focused, Some(tid(2)));
    }

    /// When a topology update removes local focus/window state, reconciliation
    /// repairs it deterministically rather than adopting the sender's focus.
    #[test]
    fn reconcile_repairs_missing_local_focus_and_invalid_active_index() {
        let mut local = Workspace::single(tid(1));
        local.add_window("2".to_owned(), tid(2));
        local.add_window("3".to_owned(), tid(9));
        let incoming = Workspace {
            windows: vec![
                crate::layout::WindowState {
                    name: "1".to_owned(),
                    state: split2(1, 4, 4),
                },
                crate::layout::WindowState {
                    name: "2".to_owned(),
                    state: split2(2, 3, 3),
                },
            ],
            active: 0,
        };
        let panes = panes_for(&[&tid(1), &tid(2), &tid(3), &tid(4), &tid(9)]);

        let out = super::reconcile_loaded_workspace(incoming, &local, Some(&tid(9)), &panes);

        assert_eq!(out.active, 1, "removed local index clamps to last window");
        assert_eq!(out.windows[0].state.focus, Some(tid(1)));
        assert_eq!(out.windows[1].state.focus, Some(tid(2)));
    }

    /// Layout tombstones retain the existing reset behavior and anchor the
    /// replacement single-pane workspace on this client's focused pane.
    #[test]
    fn layout_tombstone_resets_to_local_focused_pane() {
        use phux_protocol::wire::frame::Scope;

        let mut local = Workspace {
            windows: vec![
                crate::layout::WindowState {
                    name: "1".to_owned(),
                    state: LayoutState::single(tid(1)),
                },
                crate::layout::WindowState {
                    name: "2".to_owned(),
                    state: LayoutState::single(tid(2)),
                },
            ],
            active: 1,
        };
        let mut focused = Some(tid(2));
        let mut panes = panes_for(&[&tid(1), &tid(2)]);

        let outcome = drive_layout_frame(
            FrameKind::MetadataChanged {
                scope: Scope::Group(super::DEFAULT_GROUP_ID),
                key: crate::attach::driver::layout_key(SessionId::new(1)),
                value: None,
            },
            None,
            &mut local,
            &mut focused,
            &mut panes,
        );

        assert!(outcome.layout_replaced);
        assert_eq!(local, Workspace::single(tid(2)));
        assert_eq!(focused, Some(tid(2)));
    }

    /// A single-window workspace whose window is two leaves split
    /// side-by-side (vertical divider), with `focus` on the supplied
    /// leaf. Exercises the multi-pane render paths without a real tty.
    fn two_pane_workspace(left: &TerminalId, right: &TerminalId, focus: &TerminalId) -> Workspace {
        let state = LayoutState {
            tree: Some(LayoutNode::Split {
                dir: SplitDir::Horizontal,
                ratio: 0.5,
                left: Box::new(LayoutNode::Leaf(left.clone())),
                right: Box::new(LayoutNode::Leaf(right.clone())),
            }),
            focus: Some(focus.clone()),
        };
        Workspace {
            windows: vec![crate::layout::WindowState {
                name: "1".to_owned(),
                state,
            }],
            active: 0,
        }
    }

    fn drive_output(
        engine: &mut EngineFixture,
        out: &mut Vec<u8>,
        layout: &mut Workspace,
        focused: &mut Option<TerminalId>,
        panes: &mut HashMap<TerminalId, PaneSlot>,
        terminal_id: &TerminalId,
        bytes: &[u8],
    ) {
        let seq = engine
            .kernel
            .published(terminal_id)
            .expect("published terminal")
            .last_seq()
            .checked_add(1)
            .expect("live sequence");
        let _ = drive_output_seq(engine, out, layout, focused, panes, terminal_id, bytes, seq);
    }

    /// Like [`drive_output`] but stamps an explicit `seq` and returns the
    /// [`FrameOutcome`] so ack-emission tests can inspect `outcome.ack`.
    fn drive_output_seq(
        engine: &mut EngineFixture,
        out: &mut Vec<u8>,
        layout: &mut Workspace,
        focused: &mut Option<TerminalId>,
        panes: &mut HashMap<TerminalId, PaneSlot>,
        terminal_id: &TerminalId,
        bytes: &[u8],
        seq: u64,
    ) -> FrameOutcome {
        drive_output_seq_with_viewport(
            engine,
            out,
            layout,
            focused,
            panes,
            terminal_id,
            bytes,
            seq,
            (80, 24),
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "test driver mirrors frame inputs"
    )]
    fn drive_output_seq_with_viewport(
        engine: &mut EngineFixture,
        out: &mut Vec<u8>,
        layout: &mut Workspace,
        focused: &mut Option<TerminalId>,
        panes: &mut HashMap<TerminalId, PaneSlot>,
        terminal_id: &TerminalId,
        bytes: &[u8],
        seq: u64,
        viewport_dims: (u16, u16),
    ) -> FrameOutcome {
        let mut session_name = String::new();
        let mut zoomed: Option<TerminalId> = None;
        let mut predict = PredictionState::new(
            PredictiveConfig::disabled(),
            viewport_dims.0,
            viewport_dims.1,
        );
        let overlay = Overlay;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        handle_server_frame_with_kernel(
            &mut engine.kernel,
            &mut engine.effects,
            out,
            FrameKind::TerminalOutput {
                terminal_id: terminal_id.clone(),
                stream_id: phux_protocol::StreamId::new(1).expect("stream"),
                bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
                seq,
                bytes: bytes::Bytes::copy_from_slice(bytes),
            },
            panes,
            layout,
            focused,
            &mut zoomed,
            &mut session_name,
            None,
            None,
            viewport_dims,
            &mut predict,
            &overlay,
            None,
            &mut pending_splits,
            &mut pending_windows,
            &mut HashSet::new(),
            &mut AgentMetaIndex::default(),
            false,
            false,
        )
        .expect("handle_server_frame")
    }

    /// phux-ih39: live output that races ahead of bootstrap publication must
    /// not be interpreted against placeholder geometry. Absolute cursor
    /// movement past column 80 is the compact regression oracle.
    #[test]
    fn output_before_snapshot_uses_current_viewport_width() {
        let pane = tid(1);
        let mut layout = Workspace::single(pane.clone());
        let mut focused = Some(pane.clone());
        let (mut engine, mut panes) = published_fixture(&[(&pane, 120, 30, b"")]);
        let mut out: Vec<u8> = Vec::new();

        drive_output_seq_with_viewport(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &pane,
            b"\x1b[1;100HX",
            1,
            (120, 30),
        );

        let terminal = super::super::driver::published_terminal(&engine.kernel, &pane)
            .expect("published terminal");
        assert_eq!(terminal.cols().expect("cols"), 120);
        assert_eq!(terminal.rows().expect("rows"), 30);
        let slot = panes.get_mut(&pane).expect("slot allocated");
        let cell = slot
            .renderer
            .read_grapheme_at(terminal, 0, 99)
            .expect("read cell");
        assert_eq!(cell, Some('X'));
    }

    #[test]
    fn synchronized_output_paints_only_after_end_across_frames() {
        let pane = tid(1);
        let mut layout = Workspace::single(pane.clone());
        let mut focused = Some(pane.clone());
        let (mut engine, mut panes) = published_fixture(&[(&pane, 80, 24, b"")]);
        let mut out = Vec::new();

        drive_output(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &pane,
            b"\x1b[?2026hhalf-drawn",
        );
        assert!(out.is_empty(), "begin/body must update only the mirror");
        assert!(panes[&pane].sync_output_since.is_some());

        drive_output(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &pane,
            b" frame\x1b[?2026l",
        );
        assert!(!out.is_empty(), "end must publish the completed frame");
        assert!(panes[&pane].sync_output_since.is_none());
        let printable = strip_csi(&String::from_utf8_lossy(&out));
        assert!(printable.contains("half-drawn frame"));
    }

    /// phux-foz.9: an OSC 0/2 title riding in ordinary `TERMINAL_OUTPUT`
    /// bytes is the only identity signal a plain `claude`/`codex` pane
    /// emits — the frame must raise `chrome_dirty` when the title moves so
    /// the driver refreshes the window labels and the sidebar's agents
    /// section (the live repro: run `claude` in a pane, the agent row must
    /// appear without waiting for an unrelated chrome event; after exit,
    /// the shell's title reset must remove it the same way).
    #[test]
    fn output_title_change_marks_chrome_dirty() {
        let pane = tid(1);
        let mut layout = Workspace::single(pane.clone());
        let mut focused = Some(pane.clone());
        let (mut engine, mut panes) = published_fixture(&[(&pane, 80, 24, b"")]);
        let mut out: Vec<u8> = Vec::new();

        let plain = drive_output_seq(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &pane,
            b"just glyphs, no title",
            1,
        );
        assert!(
            !plain.chrome_dirty,
            "output that never touches the title must not repaint the chrome"
        );

        let set = drive_output_seq(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &pane,
            b"\x1b]2;\xe2\x9c\xb3 claude\x07",
            2,
        );
        assert!(
            set.chrome_dirty,
            "a new OSC 2 title must mark the chrome dirty"
        );

        let unchanged = drive_output_seq(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &pane,
            b"\x1b]2;\xe2\x9c\xb3 claude\x07more glyphs",
            3,
        );
        assert!(
            !unchanged.chrome_dirty,
            "re-asserting the same title must not repaint the chrome"
        );

        let cleared = drive_output_seq(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &pane,
            b"\x1b]2;\x07",
            4,
        );
        assert!(
            cleared.chrome_dirty,
            "clearing the title (the agent exited; the shell reset it) must repaint the chrome"
        );
    }

    /// phux-foz.9: the symmetric bootstrap path — a resync
    /// replays the pane's title too, so a previously unseen title raises
    /// `chrome_dirty` exactly like the output hot path.
    #[test]
    fn snapshot_title_change_marks_chrome_dirty() {
        let pane = tid(1);
        let mut layout = Workspace::single(pane.clone());
        let mut focused = Some(pane.clone());
        let (mut engine, mut panes) = published_fixture(&[(&pane, 80, 24, b"")]);
        let mut out: Vec<u8> = Vec::new();

        let first = drive_snapshot(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &pane,
            80,
            24,
            b"\x1b]2;codex\x07resynced",
            (80, 24),
        );
        assert!(
            first.chrome_dirty,
            "a snapshot carrying a new title must mark the chrome dirty"
        );

        let repeat = drive_snapshot(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &pane,
            80,
            24,
            b"\x1b]2;codex\x07resynced again",
            (80, 24),
        );
        assert!(
            !repeat.chrome_dirty,
            "an unchanged title replay must not repaint the chrome"
        );
    }

    #[test]
    fn snapshot_during_synchronized_output_waits_for_live_end() {
        let pane = tid(1);
        let mut layout = Workspace::single(pane.clone());
        let mut focused = Some(pane.clone());
        let (mut engine, mut panes) = published_fixture(&[(&pane, 80, 24, b"")]);
        let mut out = Vec::new();

        drive_output(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &pane,
            b"\x1b[?2026hpartial",
        );
        drive_snapshot(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &pane,
            80,
            24,
            b"\x1b[!p\x1b[2J\x1b[Hstable snapshot",
            (80, 24),
        );
        assert!(
            !out.is_empty(),
            "replacement publication must paint the new atomic replica"
        );
        assert!(
            panes[&pane].sync_output_since.is_none(),
            "synchronized-output state belongs to the retired replica"
        );
    }

    /// phux-ih39: the ATTACHED graph already carries per-pane dimensions.
    /// Seed slots from that graph so pre-bootstrap output doesn't get
    /// interpreted at 80x24.
    #[test]
    fn attached_seeds_pane_slots_from_snapshot_dimensions() {
        let pane = tid(1);
        let window = WindowId::new(1);
        let session = SessionId::new(1);
        let snapshot = SessionSnapshot::new(session, window, pane.clone())
            .with_panes(vec![TerminalInfo::new(pane.clone(), window, 132, 43)]);
        let mut panes = HashMap::new();
        let mut workspace = Workspace::default();
        let mut focused = None;
        let mut zoomed: Option<TerminalId> = None;
        let mut session_name = String::new();
        let mut predict = PredictionState::new(PredictiveConfig::disabled(), 132, 43);
        let overlay = Overlay;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut out: Vec<u8> = Vec::new();

        handle_server_frame(
            &mut out,
            FrameKind::Attached {
                attach_id: 1,
                snapshot,
                initial_client_id: ClientId::new(1),
            },
            &mut panes,
            &mut workspace,
            &mut focused,
            &mut zoomed,
            &mut session_name,
            None,
            None,
            (132, 43),
            &mut predict,
            &overlay,
            None,
            &mut pending_splits,
            &mut pending_windows,
            &mut HashSet::new(),
            &mut AgentMetaIndex::default(),
            false,
            false,
        )
        .expect("attached");

        let slot = panes.get_mut(&pane).expect("slot seeded");
        assert_eq!(slot.terminal.cols().expect("cols"), 132);
        assert_eq!(slot.terminal.rows().expect("rows"), 43);
    }

    /// `SynthesizedVtRaw` live output is applied but does not request an
    /// acknowledgement; cumulative frame ACKs belong to state-sync streams.
    #[test]
    fn synthesized_raw_output_does_not_yield_frame_ack() {
        let left = tid(1);
        let right = tid(2);
        let mut layout = two_pane_workspace(&left, &right, &left);
        let mut focused = Some(left.clone());
        let (mut engine, mut panes) =
            published_fixture(&[(&left, 80, 24, b""), (&right, 80, 24, b"")]);

        let mut out: Vec<u8> = Vec::new();
        let outcome = drive_output_seq(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &right,
            b"hi",
            1,
        );
        assert_eq!(
            outcome.ack, None,
            "raw synthesized output must not emit a state-sync acknowledgement"
        );
    }

    /// A zero sequence is not a live output sentinel in the session kernel:
    /// the published generation starts at `base_seq == 0` and therefore
    /// requires its first live payload to carry sequence 1.
    #[test]
    fn terminal_output_seq_zero_is_rejected() {
        let pane = tid(1);
        let (mut engine, _) = published_fixture(&[(&pane, 80, 24, b"")]);
        let route = route_engine_frame(
            &FrameKind::TerminalOutput {
                terminal_id: pane,
                stream_id: phux_protocol::StreamId::new(1).expect("stream"),
                bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
                seq: 0,
                bytes: bytes::Bytes::from_static(b"hi"),
            },
            &mut engine.kernel,
            &mut engine.effects,
        );
        assert!(
            route.failed.is_some(),
            "sequence zero must be rejected before rendering or acknowledgement"
        );
        assert_eq!(route.ack, None);
    }

    /// phux-2x9 via the injectable sink: a NON-focused pane must repaint
    /// on its own `TERMINAL_OUTPUT` so it isn't visually frozen. We feed
    /// output for the right (non-focused) pane and assert the captured VT
    /// carries a CUP into the right pane's rect origin plus the emitted
    /// graphemes — proving the regression without a live terminal.
    #[test]
    fn non_focused_pane_repaints_on_output() {
        let left = tid(1);
        let right = tid(2);
        let mut layout = two_pane_workspace(&left, &right, &left);
        let mut focused = Some(left.clone());
        let (mut engine, mut panes) =
            published_fixture(&[(&left, 80, 24, b""), (&right, 80, 24, b"")]);

        let mut out: Vec<u8> = Vec::new();
        drive_output(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &right,
            b"hello",
        );

        let s = String::from_utf8_lossy(&out);
        // The right pane occupies the columns after the divider in an
        // 80-col / 0.5 split: left pane cols 0..39, divider at col 40,
        // right pane from col 41 (0-based) ⇒ 1-based CUP `;42H`.
        assert!(
            s.contains(";42H"),
            "expected CUP into right pane origin (col 42); out = {s:?}"
        );
        // The renderer emits one cell at a time with an SGR delta between
        // cells, so the graphemes are interleaved with escape sequences.
        // Strip CSI sequences before the glyph check, otherwise `h`/`l`
        // would be satisfied by the cursor mode-set bytes (`\x1b[?25h` /
        // `\x1b[?25l`) rather than the pane content itself.
        let visible = strip_csi(&s);
        assert!(
            visible.contains("hello"),
            "non-focused pane should render its glyphs; visible = {visible:?}, raw = {s:?}"
        );
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "test driver mirrors frame inputs"
    )]
    fn drive_snapshot(
        engine: &mut EngineFixture,
        out: &mut Vec<u8>,
        layout: &mut Workspace,
        focused: &mut Option<TerminalId>,
        panes: &mut HashMap<TerminalId, PaneSlot>,
        terminal_id: &TerminalId,
        cols: u16,
        rows: u16,
        vt_replay_bytes: &[u8],
        viewport_dims: (u16, u16),
    ) -> FrameOutcome {
        let published = engine
            .kernel
            .published(terminal_id)
            .expect("published test generation");
        let stream_id = published.key().stream_id;
        let bootstrap_id = phux_protocol::BootstrapId::new(
            published.key().bootstrap_id.get().checked_add(1).expect("bootstrap id"),
        )
        .expect("next bootstrap");
        let base_seq = published.last_seq();
        let mut session_name = String::new();
        let mut zoomed = None;
        let mut predict = PredictionState::new(
            PredictiveConfig::disabled(),
            viewport_dims.0,
            viewport_dims.1,
        );
        let overlay = Overlay;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut expected_closes = HashSet::new();
        let mut agent_meta = AgentMetaIndex::default();
        let mut dispatch = |frame| {
            handle_server_frame_with_kernel(
                &mut engine.kernel,
                &mut engine.effects,
                out,
                frame,
                panes,
                layout,
                focused,
                &mut zoomed,
                &mut session_name,
                None,
                None,
                viewport_dims,
                &mut predict,
                &overlay,
                None,
                &mut pending_splits,
                &mut pending_windows,
                &mut expected_closes,
                &mut agent_meta,
                false,
                false,
            )
            .expect("handle bootstrap frame")
        };
        dispatch(FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
            cols,
            rows,
            base_seq,
        });
        dispatch(FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes::Bytes::copy_from_slice(vt_replay_bytes),
        });
        let outcome = dispatch(FrameKind::BootstrapReady {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        });
        drop(dispatch);
        if outcome.layout_replaced
            && let Some(active) = layout.render_window(zoomed.as_ref())
        {
            super::super::paint::paint_full_frame(
                out,
                active.as_ref(),
                panes,
                &engine.kernel,
                focused.as_ref(),
                viewport_dims,
                None,
                None,
                None,
                &session_name,
            );
        }
        outcome
    }

    /// phux-paer: on re-attach the server sends a bootstrap per pane; a
    /// NON-focused pane's publication must paint into its rect, or the pane
    /// renders blank while input still routes — the "screens wiped but still
    /// typable" report. The symmetric counterpart to
    /// [`non_focused_pane_repaints_on_output`].
    #[test]
    fn non_focused_pane_repaints_on_snapshot() {
        let left = tid(1);
        let right = tid(2);
        let mut layout = two_pane_workspace(&left, &right, &left);
        let mut focused = Some(left.clone());
        let (mut engine, mut panes) =
            published_fixture(&[(&left, 39, 24, b""), (&right, 39, 24, b"")]);

        let mut out: Vec<u8> = Vec::new();
        drive_snapshot(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &right,
            39,
            24,
            b"hello",
            (80, 24),
        );

        let s = String::from_utf8_lossy(&out);
        // Same geometry as the output test: 80-col / 0.5 split ⇒ right pane
        // origin at 0-based col 41 ⇒ 1-based CUP `;42H`.
        assert!(
            s.contains(";42H"),
            "expected CUP into right pane origin (col 42); out = {s:?}"
        );
        let visible = strip_csi(&s);
        assert!(
            visible.contains("hello"),
            "non-focused pane snapshot should render its glyphs; visible = {visible:?}, raw = {s:?}"
        );
    }

    /// The focused pane's snapshot still renders into its own rect — guards
    /// against the phux-paer non-focused branch regressing the focused path.
    #[test]
    fn focused_pane_repaints_on_snapshot() {
        let left = tid(1);
        let right = tid(2);
        let mut layout = two_pane_workspace(&left, &right, &left);
        let mut focused = Some(left.clone());
        let (mut engine, mut panes) =
            published_fixture(&[(&left, 39, 24, b""), (&right, 39, 24, b"")]);

        let mut out: Vec<u8> = Vec::new();
        drive_snapshot(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &left,
            39,
            24,
            b"world",
            (80, 24),
        );

        let s = String::from_utf8_lossy(&out);
        assert!(
            s.contains("\x1b[1;1H"),
            "expected CUP into left pane origin (col 1); out = {s:?}"
        );
        let visible = strip_csi(&s);
        assert!(
            visible.contains("world"),
            "focused pane snapshot should render its glyphs; visible = {visible:?}, raw = {s:?}"
        );
    }

    /// The focused pane's output renders into its own rect (column 1 for
    /// the left pane) and the captured stream is non-empty.
    #[test]
    fn focused_pane_repaints_on_output() {
        let left = tid(1);
        let right = tid(2);
        let mut layout = two_pane_workspace(&left, &right, &left);
        let mut focused = Some(left.clone());
        let (mut engine, mut panes) =
            published_fixture(&[(&left, 80, 24, b""), (&right, 80, 24, b"")]);

        let mut out: Vec<u8> = Vec::new();
        drive_output(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &left,
            b"world",
        );

        let s = String::from_utf8_lossy(&out);
        // Focused pane renders at column 1 (left pane origin). Glyphs are
        // interleaved with SGR resets, so assert on ordered chars.
        assert!(
            s.contains("\x1b[1;1H"),
            "expected CUP into left pane origin (col 1); out = {s:?}"
        );
        for ch in ['w', 'o', 'r', 'l', 'd'] {
            assert!(
                s.contains(ch),
                "focused pane glyph {ch:?} missing; out = {s:?}"
            );
        }
    }

    /// Off-screen invariant: a `TERMINAL_OUTPUT` for a pane that lives in
    /// a NON-active window must warm that pane's libghostty mirror but
    /// paint nothing (it isn't on screen). The pane has no rect in the
    /// active window's composition, so the renderer emits no CUP.
    #[test]
    fn output_for_inactive_window_pane_warms_mirror_but_does_not_paint() {
        let active_pane = tid(1);
        let other_pane = tid(2);
        // Two windows: active window holds pane 1; window 2 holds pane 2.
        let mut workspace = Workspace::single(active_pane.clone());
        workspace.add_window("2".to_owned(), other_pane.clone());
        // Re-select window 0 as active (add_window activated the new one).
        workspace.select(0);
        let mut focused = Some(active_pane.clone());
        let (mut engine, mut panes) =
            published_fixture(&[(&active_pane, 80, 24, b""), (&other_pane, 80, 24, b"")]);

        let mut out: Vec<u8> = Vec::new();
        drive_output(
            &mut engine,
            &mut out,
            &mut workspace,
            &mut focused,
            &mut panes,
            &other_pane,
            b"offscreen",
        );

        // Nothing painted: the off-screen pane has no rect in the active
        // window, so the renderer wrote no bytes at all.
        assert!(
            out.is_empty(),
            "off-screen pane must not paint; out = {:?}",
            String::from_utf8_lossy(&out),
        );
        // The mirror is warm: reading the grapheme grid back shows the
        // bytes landed in pane 2's libghostty Terminal.
        let terminal = super::super::driver::published_terminal(&engine.kernel, &other_pane)
            .expect("pane 2 terminal");
        let slot = panes.get_mut(&other_pane).expect("pane 2 slot");
        let cell = slot
            .renderer
            .read_grapheme_at(terminal, 0, 0)
            .expect("read cell");
        assert_eq!(cell, Some('o'), "pane 2 mirror should hold the output");
    }

    /// phux-4li.15: a `TERMINAL_SPAWNED` reply for a parked new-window
    /// opens a new window seeded on the spawned pane, makes it active,
    /// re-anchors focus, and asks for a broadcast + reflow.
    #[test]
    fn window_spawned_opens_active_window_focused_on_new_pane() {
        use super::handle_window_spawned;
        use crate::attach::actions::PendingWindow;
        use phux_protocol::wire::frame::SpawnResult;

        let mut workspace = Workspace::single(tid(1)); // window "1", pane 1
        let mut focused = Some(tid(1));
        let mut panes = panes_for(&[&tid(1)]);
        let mut out: Vec<u8> = Vec::new();

        let mut history = crate::attach::focus::FocusHistory::default();
        let before = focused.clone();
        let outcome = handle_window_spawned(
            &mut out,
            &mut workspace,
            &mut focused,
            &mut panes,
            &PendingWindow {
                name: "2".to_owned(),
            },
            SpawnResult::Ok(tid(2)),
        )
        .expect("handle_window_spawned");

        assert_eq!(workspace.windows.len(), 2);
        assert_eq!(workspace.active, 1, "new window is active");
        assert_eq!(workspace.windows[1].name, "2");
        history.observe(before, focused.as_ref());
        history.repair(focused.as_ref(), &workspace);
        assert_eq!(focused, Some(tid(2)), "focus follows the new pane");
        assert_eq!(
            history.target(focused.as_ref(), &workspace),
            Some(tid(1)),
            "async new-window completion records the pane being left",
        );
        assert!(panes.contains_key(&tid(2)), "new pane got a slot");
        assert!(outcome.layout_replaced && outcome.emit_set_metadata && outcome.reflow_panes);
    }

    /// Drive a `TERMINAL_SPAWNED { Ok }` reply through the full dispatcher
    /// with one parked [`PendingSplit`], returning the resulting `zoomed`
    /// state (phux-r82.7's zoom-on-spawn contract lives there).
    fn drive_spawned_with_pending_split(zoom_on_spawn: bool) -> Option<TerminalId> {
        use crate::attach::actions::PendingSplit;
        use phux_protocol::wire::frame::SpawnResult;

        let anchor = tid(1);
        let mut workspace = Workspace::single(anchor.clone());
        let mut focused = Some(anchor.clone());
        let mut panes = panes_for(&[&anchor]);
        let mut out: Vec<u8> = Vec::new();
        let mut session_name = String::new();
        let mut zoomed: Option<TerminalId> = Some(anchor.clone());
        let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let mut pending_splits = HashMap::new();
        pending_splits.insert(
            7,
            PendingSplit {
                focused_at_request: anchor,
                dir: SplitDir::Horizontal,
                zoom_on_spawn,
            },
        );
        let mut pending_windows = HashMap::new();
        let mut history = crate::attach::focus::FocusHistory::default();
        let before = focused.clone();
        let outcome = handle_server_frame(
            &mut out,
            FrameKind::TerminalSpawned {
                request_id: 7,
                result: SpawnResult::Ok(tid(2)),
            },
            &mut panes,
            &mut workspace,
            &mut focused,
            &mut zoomed,
            &mut session_name,
            None,
            None,
            (80, 24),
            &mut predict,
            &overlay,
            None,
            &mut pending_splits,
            &mut pending_windows,
            &mut HashSet::new(),
            &mut AgentMetaIndex::default(),
            false,
            false,
        )
        .expect("handle_server_frame");
        history.observe(before, focused.as_ref());
        history.repair(focused.as_ref(), &workspace);
        assert!(outcome.layout_replaced, "split reply replaces the layout");
        assert_eq!(focused, Some(tid(2)), "focus follows the spawned pane");
        assert_eq!(
            history.target(focused.as_ref(), &workspace),
            Some(tid(1)),
            "full async split reply records the anchor as MRU",
        );
        zoomed
    }

    /// phux-r82.7: a parked split with `zoom_on_spawn` zooms the freshly
    /// spawned pane (placement = "zoomed" plugin panes).
    #[test]
    fn terminal_spawned_zoom_on_spawn_zooms_the_new_pane() {
        assert_eq!(drive_spawned_with_pending_split(true), Some(tid(2)));
    }

    /// phux-x2hm parity guard: a plain split still un-zooms.
    #[test]
    fn terminal_spawned_without_zoom_on_spawn_clears_zoom() {
        assert_eq!(drive_spawned_with_pending_split(false), None);
    }

    /// phux-flywheel: the apply-vs-paint split is observable. Driving a
    /// `TERMINAL_OUTPUT` for the focused pane under a debug-level capturing
    /// subscriber must close BOTH child spans — `vt_apply` (libghostty
    /// parse) and `paint_trigger` (render) — so a trace can attribute
    /// client lag to apply-ms vs paint-ms separately. We assert on
    /// span-close events (the parse + render each report their own busy
    /// time) rather than the fused parent `handle_server_frame` close.
    #[test]
    fn output_emits_separate_apply_and_paint_spans() {
        use std::sync::Arc;
        use tracing_subscriber::fmt::MakeWriter;
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::{Registry, fmt};

        #[derive(Clone, Default)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buf {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().expect("lock").extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for Buf {
            type Writer = Self;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let _guard = TRACE_TEST_LOCK.lock().expect("trace test lock");

        let buf = Buf::default();
        let layer = fmt::layer()
            .with_ansi(false)
            .with_writer(buf.clone())
            .with_span_events(fmt::format::FmtSpan::CLOSE);
        let subscriber = Registry::default().with(layer);

        {
            tracing::subscriber::set_global_default(subscriber)
                .expect("install test tracing subscriber");
            tracing_core::callsite::rebuild_interest_cache();
            let left = tid(1);
            let right = tid(2);
            let mut layout = two_pane_workspace(&left, &right, &left);
            let mut focused = Some(left.clone());
            let (mut engine, mut panes) =
                published_fixture(&[(&left, 80, 24, b""), (&right, 80, 24, b"")]);
            let mut out: Vec<u8> = Vec::new();
            // Drive the focused pane so the paint trigger fires.
            drive_output(
                &mut engine,
                &mut out,
                &mut layout,
                &mut focused,
                &mut panes,
                &left,
                b"hi",
            );
        }

        let log = String::from_utf8(buf.0.lock().expect("lock").clone()).expect("utf8");
        // Both child spans must have closed (FmtSpan::CLOSE prints a
        // `close` line carrying `time.busy` per span name).
        assert!(
            log.contains("vt_apply"),
            "vt_apply span never closed; log:\n{log}"
        );
        assert!(
            log.contains("paint_trigger"),
            "paint_trigger span never closed; log:\n{log}"
        );
        // And the parent fused span is still present (apply+paint).
        assert!(
            log.contains("handle_server_frame"),
            "parent span missing; log:\n{log}"
        );
    }

    /// A `Bell` frame routes a BEL byte through the injected sink, so a
    /// headless capture (and a future agent surface) can observe it.
    #[test]
    fn bell_frame_writes_bel_to_sink() {
        let mut layout = Workspace::single(tid(1));
        let mut focused = Some(tid(1));
        let mut zoomed: Option<TerminalId> = None;
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut session_name = String::new();
        let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();

        let mut out: Vec<u8> = Vec::new();
        handle_server_frame(
            &mut out,
            FrameKind::Bell {
                terminal_id: tid(1),
            },
            &mut panes,
            &mut layout,
            &mut focused,
            &mut zoomed,
            &mut session_name,
            None,
            None,
            (80, 24),
            &mut predict,
            &overlay,
            None,
            &mut pending_splits,
            &mut pending_windows,
            &mut HashSet::new(),
            &mut AgentMetaIndex::default(),
            false,
            false,
        )
        .expect("handle_server_frame");

        assert_eq!(&out, b"\x07", "bell must emit a single BEL byte");
    }

    /// Drive a `TERMINAL_CLOSED { terminal_id, exit_status }` through
    /// [`handle_server_frame`] and return the resulting [`FrameOutcome`]
    /// so the consumer-side detach policy (phux-4r1) can be asserted.
    fn drive_closed(
        layout: &mut Workspace,
        focused: &mut Option<TerminalId>,
        panes: &mut HashMap<TerminalId, PaneSlot>,
        terminal_id: &TerminalId,
        exit_status: Option<i32>,
    ) -> FrameOutcome {
        drive_closed_expecting(
            layout,
            focused,
            panes,
            terminal_id,
            exit_status,
            &mut HashSet::new(),
        )
    }

    /// [`drive_closed`] with a caller-owned `expected_closes` set, so the
    /// phux-i0e8.2.2 suppress-and-drain contract can be asserted.
    fn drive_closed_expecting(
        layout: &mut Workspace,
        focused: &mut Option<TerminalId>,
        panes: &mut HashMap<TerminalId, PaneSlot>,
        terminal_id: &TerminalId,
        exit_status: Option<i32>,
        expected_closes: &mut HashSet<TerminalId>,
    ) -> FrameOutcome {
        let mut out: Vec<u8> = Vec::new();
        let mut session_name = String::new();
        let mut zoomed: Option<TerminalId> = None;
        let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        handle_server_frame(
            &mut out,
            FrameKind::TerminalClosed {
                terminal_id: terminal_id.clone(),
                exit_status,
            },
            panes,
            layout,
            focused,
            &mut zoomed,
            &mut session_name,
            None,
            None,
            (80, 24),
            &mut predict,
            &overlay,
            None,
            &mut pending_splits,
            &mut pending_windows,
            expected_closes,
            &mut AgentMetaIndex::default(),
            false,
            false,
        )
        .expect("handle_server_frame")
    }

    /// phux-4r1: the detach policy is consumer-owned. When the LAST pane
    /// closes there is nothing left to render or route input to, so the
    /// TUI detaches itself — the `TerminalClosed` arm returns
    /// `FrameOutcome { exit: true }`. This is the consumer-side half of
    /// the EOF reshape: the server emits `TERMINAL_CLOSED` (an L1
    /// lifecycle fact) and the client decides to leave.
    #[test]
    fn last_pane_closed_detaches_the_client() {
        let pane = tid(1);
        let mut workspace = Workspace::single(pane.clone());
        let mut focused = Some(pane.clone());
        let mut panes = panes_for(&[&pane]);

        let outcome = drive_closed(&mut workspace, &mut focused, &mut panes, &pane, Some(0));

        assert!(
            outcome.exit,
            "closing the only pane must make the consumer detach (exit: true)",
        );
        assert_eq!(
            outcome.exit_reason,
            Some(AttachEnd::LastPaneClosed {
                exit_status: Some(0)
            }),
            "the exit must carry WHY: the last pane closed, with its status",
        );
        assert!(
            workspace.windows.is_empty(),
            "the workspace must have no windows left after the last pane closes",
        );
        assert!(
            !panes.contains_key(&pane),
            "the closed pane's slot must be dropped",
        );
    }

    /// phux-i0e8.2.2: a last-pane death by signal (or unknown cause)
    /// carries `exit_status: None` up as the exit reason, so the CLI can
    /// say "killed" instead of pretending the exit was clean.
    #[test]
    fn last_pane_signal_death_carries_none_status_in_exit_reason() {
        let pane = tid(1);
        let mut workspace = Workspace::single(pane.clone());
        let mut focused = Some(pane.clone());
        let mut panes = panes_for(&[&pane]);

        let outcome = drive_closed(&mut workspace, &mut focused, &mut panes, &pane, None);

        assert!(outcome.exit);
        assert_eq!(
            outcome.exit_reason,
            Some(AttachEnd::LastPaneClosed { exit_status: None }),
        );
    }

    /// Drive an `EVENT { terminal, Asked }` through [`handle_server_frame`]
    /// and return the outcome (phux-foz.1 / ADR-0035).
    fn drive_asked(
        layout: &mut Workspace,
        focused: &mut Option<TerminalId>,
        panes: &mut HashMap<TerminalId, PaneSlot>,
        terminal_id: &TerminalId,
    ) -> FrameOutcome {
        use phux_protocol::wire::frame::AgentEvent;
        let mut out: Vec<u8> = Vec::new();
        let mut session_name = String::new();
        let mut zoomed: Option<TerminalId> = None;
        let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut agent_meta = AgentMetaIndex::default();
        handle_server_frame(
            &mut out,
            FrameKind::Event {
                terminal: Some(terminal_id.clone()),
                event: AgentEvent::Asked {
                    id: "q1".to_owned(),
                    question: "deploy to prod?".to_owned(),
                    suggestions: vec!["yes".to_owned(), "no".to_owned()],
                    elapsed_seconds: None,
                },
            },
            panes,
            layout,
            focused,
            &mut zoomed,
            &mut session_name,
            None,
            None,
            (80, 24),
            &mut predict,
            &overlay,
            None,
            &mut pending_splits,
            &mut pending_windows,
            &mut HashSet::new(),
            &mut agent_meta,
            false,
            false,
        )
        .expect("handle_server_frame")
    }

    /// phux-foz.1: an ADR-0035 `Asked` event raises the pane's attention
    /// flag and asks the driver to repaint the chrome — including for a
    /// NON-focused pane (the whole point is surfacing a question the user
    /// is not looking at).
    #[test]
    fn asked_event_sets_attention_and_dirties_chrome() {
        let left = tid(1);
        let right = tid(2);
        let mut layout = two_pane_workspace(&left, &right, &left);
        let mut focused = Some(left.clone());
        let mut panes = panes_for(&[&left, &right]);

        let outcome = drive_asked(&mut layout, &mut focused, &mut panes, &right);

        assert!(
            panes.get(&right).expect("slot").attention,
            "the asking pane's attention flag must raise"
        );
        assert!(
            !panes.get(&left).expect("slot").attention,
            "the other pane stays quiet"
        );
        assert!(outcome.chrome_dirty, "the chrome must repaint");
    }

    /// phux-foz.1: a repeated `Asked` while the flag is already up changes
    /// no visible state, so it must not request another repaint.
    #[test]
    fn repeated_asked_event_does_not_redirty_chrome() {
        let pane = tid(1);
        let mut layout = Workspace::single(pane.clone());
        let mut focused = Some(pane.clone());
        let mut panes = panes_for(&[&pane]);

        let first = drive_asked(&mut layout, &mut focused, &mut panes, &pane);
        assert!(first.chrome_dirty);
        let second = drive_asked(&mut layout, &mut focused, &mut panes, &pane);
        assert!(
            !second.chrome_dirty,
            "an already-flagged pane must not force a repaint"
        );
        assert!(panes.get(&pane).expect("slot").attention, "flag stays up");
    }

    /// phux-foz.1: an `Asked` for a pane with no slot yet (it can precede
    /// the first snapshot) is dropped without a repaint, mirroring the
    /// early-`TerminalControl` policy.
    #[test]
    fn asked_event_for_unknown_pane_is_dropped() {
        let known = tid(1);
        let unknown = tid(9);
        let mut layout = Workspace::single(known.clone());
        let mut focused = Some(known.clone());
        let mut panes = panes_for(&[&known]);

        let outcome = drive_asked(&mut layout, &mut focused, &mut panes, &unknown);

        assert!(!outcome.chrome_dirty, "no slot, nothing to repaint");
        assert!(
            !panes.contains_key(&unknown),
            "no slot is allocated for an event-only pane"
        );
    }

    /// phux-foz.4: drive one agent event through [`handle_server_frame`]
    /// with minimal single-pane scaffolding; returns the outcome.
    fn drive_event(
        panes: &mut HashMap<TerminalId, PaneSlot>,
        terminal_id: &TerminalId,
        event: phux_protocol::wire::frame::AgentEvent,
    ) -> FrameOutcome {
        let mut layout = Workspace::single(terminal_id.clone());
        let mut focused = Some(terminal_id.clone());
        let mut out: Vec<u8> = Vec::new();
        let mut session_name = String::new();
        let mut zoomed: Option<TerminalId> = None;
        let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut agent_meta = AgentMetaIndex::default();
        handle_server_frame(
            &mut out,
            FrameKind::Event {
                terminal: Some(terminal_id.clone()),
                event,
            },
            panes,
            &mut layout,
            &mut focused,
            &mut zoomed,
            &mut session_name,
            None,
            None,
            (80, 24),
            &mut predict,
            &overlay,
            None,
            &mut pending_splits,
            &mut pending_windows,
            &mut HashSet::new(),
            &mut agent_meta,
            false,
            false,
        )
        .expect("handle_server_frame")
    }

    /// phux-foz.4: a `cwd_changed` event lands in the pane's slot and
    /// dirties the chrome; repeating the same directory is a no-op.
    #[test]
    fn cwd_changed_event_updates_slot_and_coalesces() {
        use phux_protocol::wire::frame::AgentEvent;
        let pane = tid(1);
        let mut panes = panes_for(&[&pane]);

        let first = drive_event(
            &mut panes,
            &pane,
            AgentEvent::CwdChanged {
                cwd: "/tmp/work".to_owned(),
            },
        );
        assert!(first.chrome_dirty, "a new cwd must repaint the chrome");
        assert_eq!(
            panes.get(&pane).expect("slot").cwd.as_deref(),
            Some("/tmp/work")
        );

        let repeat = drive_event(
            &mut panes,
            &pane,
            AgentEvent::CwdChanged {
                cwd: "/tmp/work".to_owned(),
            },
        );
        assert!(!repeat.chrome_dirty, "unchanged cwd must not repaint");
    }

    /// phux-foz.4: a `command_finished` event records the exit code (and a
    /// later code replaces it); an unchanged value is a no-op.
    #[test]
    fn command_finished_event_records_last_exit() {
        use phux_protocol::wire::frame::AgentEvent;
        let pane = tid(1);
        let mut panes = panes_for(&[&pane]);
        assert_eq!(panes.get(&pane).expect("slot").last_exit, None);

        let first = drive_event(
            &mut panes,
            &pane,
            AgentEvent::CommandFinished { exit_code: Some(0) },
        );
        assert!(first.chrome_dirty);
        assert_eq!(panes.get(&pane).expect("slot").last_exit, Some(0));

        let repeat = drive_event(
            &mut panes,
            &pane,
            AgentEvent::CommandFinished { exit_code: Some(0) },
        );
        assert!(!repeat.chrome_dirty, "same code must not repaint");

        let failed = drive_event(
            &mut panes,
            &pane,
            AgentEvent::CommandFinished {
                exit_code: Some(127),
            },
        );
        assert!(failed.chrome_dirty);
        assert_eq!(panes.get(&pane).expect("slot").last_exit, Some(127));
    }

    /// phux-foz.4: cwd/exit events for a pane with no slot yet are dropped
    /// without a repaint, mirroring the early-`TerminalControl` policy.
    #[test]
    fn cwd_and_exit_events_for_unknown_pane_are_dropped() {
        use phux_protocol::wire::frame::AgentEvent;
        let known = tid(1);
        let unknown = tid(9);
        let mut panes = panes_for(&[&known]);

        let cwd = drive_event(
            &mut panes,
            &unknown,
            AgentEvent::CwdChanged {
                cwd: "/x".to_owned(),
            },
        );
        let exit = drive_event(
            &mut panes,
            &unknown,
            AgentEvent::CommandFinished { exit_code: Some(1) },
        );
        assert!(!cwd.chrome_dirty && !exit.chrome_dirty);
        assert!(!panes.contains_key(&unknown));
    }

    /// phux-i0e8.2.1: a `TerminalControl` event carrying `holder` and a
    /// running lifecycle.
    fn control_event(holder: Option<ClientId>) -> phux_protocol::wire::frame::AgentEvent {
        use phux_protocol::wire::frame::{AgentEvent, ControlAction, TerminalLifecycle};
        AgentEvent::TerminalControl {
            lifecycle: TerminalLifecycle::Running,
            exit_status: None,
            input_holder: holder,
            action: match holder {
                Some(_) => ControlAction::Acquired,
                None => ControlAction::Released,
            },
            actor: holder,
        }
    }

    /// phux-i0e8.2.1: drive one frame through [`handle_server_frame`]
    /// with an explicit focused pane (which `drive_event` pins to the
    /// event's own terminal), for the input-authority notice tests.
    fn drive_frame_focused(
        panes: &mut HashMap<TerminalId, PaneSlot>,
        focused_id: &TerminalId,
        frame: FrameKind,
    ) -> FrameOutcome {
        let mut layout = Workspace::single(focused_id.clone());
        let mut focused = Some(focused_id.clone());
        let mut out: Vec<u8> = Vec::new();
        let mut session_name = String::new();
        let mut zoomed: Option<TerminalId> = None;
        let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        let mut agent_meta = AgentMetaIndex::default();
        handle_server_frame(
            &mut out,
            frame,
            panes,
            &mut layout,
            &mut focused,
            &mut zoomed,
            &mut session_name,
            None,
            None,
            (80, 24),
            &mut predict,
            &overlay,
            None,
            &mut pending_splits,
            &mut pending_windows,
            &mut HashSet::new(),
            &mut agent_meta,
            false,
            false,
        )
        .expect("handle_server_frame")
    }

    /// phux-i0e8.2.1 acceptance (a): a focused-pane input-authority holder
    /// TRANSITION yields the expected notice; the attach-time initial
    /// state (the first `TerminalControl` a slot sees) yields none.
    #[test]
    fn focused_holder_transition_yields_a_notice_and_initial_state_does_not() {
        use crate::render::chrome::status_bar::NoticeSeverity;
        let pane = tid(1);
        let mut panes = panes_for(&[&pane]);
        let holder = ClientId::new(9);

        // Attach-time initial state: first control event folds silently.
        let initial = drive_event(&mut panes, &pane, control_event(Some(holder)));
        assert!(initial.chrome_dirty, "the badge still refreshes");
        assert!(
            initial.notices.is_empty(),
            "the attach-time initial state must not raise a notice"
        );
        assert_eq!(panes.get(&pane).expect("slot").input_holder, Some(holder));

        // A later holder change is a transition: notice raised.
        let released = drive_event(&mut panes, &pane, control_event(None));
        assert_eq!(released.notices.len(), 1, "one notice per transition");
        assert_eq!(released.notices[0].severity, NoticeSeverity::Info);
        assert_eq!(released.notices[0].text, "input: wheel released");

        let seized = drive_event(&mut panes, &pane, control_event(Some(holder)));
        assert_eq!(seized.notices.len(), 1);
        assert_eq!(seized.notices[0].text, "input: c9 took the wheel");

        // A control event that does NOT move the holder (e.g. a freeze)
        // is not an authority transition: no notice.
        let same = drive_event(&mut panes, &pane, control_event(Some(holder)));
        assert!(
            same.notices.is_empty(),
            "an unchanged holder must not raise a notice"
        );
    }

    /// phux-i0e8.2.1: a holder transition on an UNFOCUSED pane refreshes
    /// the chrome but raises no notice — the transient slot is scoped to
    /// the pane the user is typing into.
    #[test]
    fn unfocused_holder_transition_yields_no_notice() {
        let focused = tid(1);
        let background = tid(2);
        let mut panes = panes_for(&[&focused, &background]);
        let holder = ClientId::new(4);

        // Seed the background pane's initial control state, then transition.
        let _ = drive_frame_focused(
            &mut panes,
            &focused,
            FrameKind::Event {
                terminal: Some(background.clone()),
                event: control_event(None),
            },
        );
        let outcome = drive_frame_focused(
            &mut panes,
            &focused,
            FrameKind::Event {
                terminal: Some(background.clone()),
                event: control_event(Some(holder)),
            },
        );
        assert!(outcome.chrome_dirty, "the badge state still folds");
        assert!(
            outcome.notices.is_empty(),
            "a background pane's handover must not steal the notice slot"
        );
        assert_eq!(
            panes.get(&background).expect("slot").input_holder,
            Some(holder),
        );
    }

    /// phux-i0e8.2.1 acceptance (b): an uncorrelated
    /// `ERROR { SATELLITE_UNREACHABLE }` — the hub announcing a
    /// degraded-federation transition — yields a Warn notice; the
    /// correlated shape stays on its request/reply path (no notice).
    #[test]
    fn degraded_federation_transition_yields_a_warn_notice() {
        use crate::render::chrome::status_bar::NoticeSeverity;
        use phux_protocol::wire::frame::ErrorCode;
        let pane = tid(1);
        let mut panes = panes_for(&[&pane]);

        let outcome = drive_frame_focused(
            &mut panes,
            &pane,
            FrameKind::Error {
                request_id: None,
                code: ErrorCode::SatelliteUnreachable,
                message: "satellite gpubox unreachable".to_owned(),
            },
        );
        assert_eq!(outcome.notices.len(), 1);
        assert_eq!(outcome.notices[0].severity, NoticeSeverity::Warn);
        assert_eq!(
            outcome.notices[0].text,
            "federation degraded: satellite gpubox unreachable",
        );

        let correlated = drive_frame_focused(
            &mut panes,
            &pane,
            FrameKind::Error {
                request_id: Some(7),
                code: ErrorCode::SatelliteUnreachable,
                message: "satellite gpubox unreachable".to_owned(),
            },
        );
        assert!(
            correlated.notices.is_empty(),
            "a correlated satellite error belongs to its request, not the notice slot"
        );
    }

    /// phux-4r1: closing one of several panes is NOT a detach. The
    /// survivor stays attached — the `TerminalClosed` arm folds the
    /// closed leaf out, re-anchors focus, and asks for a repaint +
    /// reflow + broadcast, with `exit: false`.
    #[test]
    fn closing_one_of_several_panes_keeps_the_client_attached() {
        let left = tid(1);
        let right = tid(2);
        let mut workspace = two_pane_workspace(&left, &right, &left);
        let mut focused = Some(left.clone());
        let mut panes = panes_for(&[&left, &right]);

        let outcome = drive_closed(&mut workspace, &mut focused, &mut panes, &left, Some(0));

        assert!(
            !outcome.exit,
            "a surviving pane means the client stays attached (exit: false)",
        );
        assert_eq!(
            workspace.windows.len(),
            1,
            "the window survives with the remaining pane",
        );
        assert_eq!(
            focused,
            Some(right),
            "focus re-anchors onto the surviving leaf",
        );
        assert!(
            outcome.layout_replaced && outcome.emit_set_metadata && outcome.reflow_panes,
            "the fold triggers repaint + sibling broadcast + survivor reflow",
        );
        assert!(
            outcome.notices.is_empty(),
            "a clean exit 0 is the user typing `exit` — no notice",
        );
    }

    /// phux-i0e8.2.2: a surviving layout gets a transient Warn notice when
    /// a sibling pane dies with a non-zero status — the OOM-killed / crashed
    /// process must not vanish silently while the fold animates over it.
    #[test]
    fn survivor_close_with_nonzero_status_raises_warn_notice() {
        use crate::render::chrome::status_bar::NoticeSeverity;
        let left = tid(1);
        let right = tid(2);
        let mut workspace = two_pane_workspace(&left, &right, &left);
        let mut focused = Some(left.clone());
        let mut panes = panes_for(&[&left, &right]);

        let outcome = drive_closed(&mut workspace, &mut focused, &mut panes, &left, Some(137));

        assert_eq!(outcome.notices.len(), 1, "exactly one notice per close");
        assert_eq!(outcome.notices[0].severity, NoticeSeverity::Warn);
        assert_eq!(outcome.notices[0].text, "pane 1: exited 137");
    }

    /// phux-i0e8.2.2: `exit_status: None` (signal kill / unknown) names the
    /// shape rather than inventing a code.
    #[test]
    fn survivor_close_by_signal_names_the_kill_shape() {
        use crate::render::chrome::status_bar::NoticeSeverity;
        let left = tid(1);
        let right = tid(2);
        let mut workspace = two_pane_workspace(&left, &right, &left);
        let mut focused = Some(left.clone());
        let mut panes = panes_for(&[&left, &right]);

        let outcome = drive_closed(&mut workspace, &mut focused, &mut panes, &right, None);

        assert_eq!(outcome.notices.len(), 1);
        assert_eq!(outcome.notices[0].severity, NoticeSeverity::Warn);
        assert_eq!(
            outcome.notices[0].text,
            "pane 2: killed (signal or unknown)"
        );
    }

    /// phux-i0e8.2.2: a close THIS client requested (kill-pane /
    /// kill-window parked the id in `expected_closes`) is suppressed —
    /// and the marker is DRAINED, so a later spontaneous death of the
    /// same id would notify again.
    #[test]
    fn expected_close_suppresses_notice_and_drains_the_marker() {
        let left = tid(1);
        let right = tid(2);
        let mut workspace = two_pane_workspace(&left, &right, &left);
        let mut focused = Some(left.clone());
        let mut panes = panes_for(&[&left, &right]);
        let mut expected: HashSet<TerminalId> = HashSet::new();
        expected.insert(left.clone());

        let outcome = drive_closed_expecting(
            &mut workspace,
            &mut focused,
            &mut panes,
            &left,
            Some(137),
            &mut expected,
        );

        assert!(
            outcome.notices.is_empty(),
            "a client-initiated kill is not news to the client",
        );
        assert!(
            expected.is_empty(),
            "the expectation must be consumed by the close it predicted",
        );
    }

    /// phux-i0e8.2.2: one wording for every exit shape, shared by the
    /// survivor notice and the last-pane explanation.
    #[test]
    fn describe_exit_covers_all_shapes() {
        assert_eq!(super::describe_exit(Some(0)), "exited 0");
        assert_eq!(super::describe_exit(Some(137)), "exited 137");
        assert_eq!(super::describe_exit(Some(-1)), "exited -1");
        assert_eq!(super::describe_exit(None), "killed (signal or unknown)");
    }

    #[test]
    fn closing_the_mru_pane_clears_stale_history() {
        let left = tid(1);
        let right = tid(2);
        let mut workspace = two_pane_workspace(&left, &right, &left);
        let mut focused = Some(left.clone());
        let mut panes = panes_for(&[&left, &right]);
        let mut history = crate::attach::focus::FocusHistory::with_previous(right.clone());

        let before = focused.clone();
        let _ = drive_closed(&mut workspace, &mut focused, &mut panes, &right, Some(0));
        history.observe(before, focused.as_ref());
        history.repair(focused.as_ref(), &workspace);

        assert_eq!(
            history.previous(),
            None,
            "closed MRU target must be cleared"
        );
    }

    /// ADR-0040: drive one frame through [`handle_server_frame`] with a
    /// caller-owned [`AgentMetaIndex`], for the agent-metadata arms.
    fn drive_meta_frame(frame: FrameKind, agent_meta: &mut AgentMetaIndex) -> FrameOutcome {
        let pane = tid(1);
        let mut layout = Workspace::single(pane.clone());
        let mut focused = Some(pane);
        let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
        let mut out: Vec<u8> = Vec::new();
        let mut session_name = String::new();
        let mut zoomed: Option<TerminalId> = None;
        let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();
        handle_server_frame(
            &mut out,
            frame,
            &mut panes,
            &mut layout,
            &mut focused,
            &mut zoomed,
            &mut session_name,
            None,
            None,
            (80, 24),
            &mut predict,
            &overlay,
            None,
            &mut pending_splits,
            &mut pending_windows,
            &mut HashSet::new(),
            agent_meta,
            false,
            false,
        )
        .expect("handle_server_frame")
    }

    /// ADR-0040: a subscribed `phux.agent/v1` broadcast decodes into the
    /// index and flags the chrome refresh; the tombstone (DELETE) clears
    /// the record so labels fall back to the OSC-title path.
    #[test]
    fn agent_metadata_broadcast_updates_index_and_tombstone_clears_it() {
        use phux_protocol::wire::frame::{Scope, TERMINAL_AGENT_KEY};
        let pane = tid(1);
        let mut agent_meta = AgentMetaIndex::default();

        let outcome = drive_meta_frame(
            FrameKind::MetadataChanged {
                scope: Scope::Terminal(pane.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
                value: Some(br#"{"name":"reviewer","state":"blocked"}"#.to_vec()),
            },
            &mut agent_meta,
        );
        assert!(outcome.agent_meta_changed, "a new record must flag chrome");
        let record = agent_meta.records.get(&pane).expect("record stored");
        assert_eq!(record.name, "reviewer");
        assert_eq!(record.state, crate::agent_meta::AgentMetaState::Blocked);

        // Re-asserting the identical record is a no-op (no repaint churn).
        let outcome = drive_meta_frame(
            FrameKind::MetadataChanged {
                scope: Scope::Terminal(pane.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
                value: Some(br#"{"name":"reviewer","state":"blocked"}"#.to_vec()),
            },
            &mut agent_meta,
        );
        assert!(
            !outcome.agent_meta_changed,
            "identical record must not flag"
        );

        // Tombstone (DELETE_METADATA) clears the record.
        let outcome = drive_meta_frame(
            FrameKind::MetadataChanged {
                scope: Scope::Terminal(pane.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
                value: None,
            },
            &mut agent_meta,
        );
        assert!(outcome.agent_meta_changed, "a cleared record must flag");
        assert!(!agent_meta.records.contains_key(&pane));
    }

    /// ADR-0040: a `GET_METADATA` reply correlated through
    /// `AgentMetaIndex::pending` seeds the record for a pane whose agent
    /// declared itself before we attached; an absent key (`value: None`)
    /// resolves the pending entry without inventing a record.
    #[test]
    fn agent_metadata_get_reply_is_correlated_by_request_id() {
        let pane = tid(1);
        let mut agent_meta = AgentMetaIndex::default();
        agent_meta.pending.insert(77, pane.clone());

        let outcome = drive_meta_frame(
            FrameKind::MetadataValue {
                request_id: 77,
                value: Some(br#"{"name":"codex","kind":"codex","state":"working"}"#.to_vec()),
            },
            &mut agent_meta,
        );
        assert!(outcome.agent_meta_changed);
        assert!(agent_meta.pending.is_empty(), "pending entry consumed");
        assert_eq!(agent_meta.records.get(&pane).expect("record").name, "codex");

        agent_meta.pending.insert(78, pane);
        let outcome = drive_meta_frame(
            FrameKind::MetadataValue {
                request_id: 78,
                value: None,
            },
            &mut agent_meta,
        );
        assert!(outcome.agent_meta_changed, "absent key clears the record");
        assert!(agent_meta.records.is_empty());
    }

    /// phux-foz.5: the `phux.config.reload/v1` doorbell flags a config
    /// reload on a non-tombstone Global broadcast; tombstones and
    /// non-Global scopes do not ring it.
    #[test]
    fn config_reload_doorbell_flags_reload_and_ignores_tombstones() {
        use phux_protocol::wire::frame::{CONFIG_RELOAD_KEY, Scope};
        let mut agent_meta = AgentMetaIndex::default();

        let outcome = drive_meta_frame(
            FrameKind::MetadataChanged {
                scope: Scope::Global,
                key: CONFIG_RELOAD_KEY.to_owned(),
                value: Some(b"1234-99".to_vec()),
            },
            &mut agent_meta,
        );
        assert!(outcome.config_reload, "the doorbell must flag a reload");
        assert!(
            !outcome.layout_replaced && !outcome.agent_meta_changed,
            "the doorbell must not masquerade as a layout or agent change",
        );

        // Tombstone (DELETE_METADATA): not a reload request.
        let outcome = drive_meta_frame(
            FrameKind::MetadataChanged {
                scope: Scope::Global,
                key: CONFIG_RELOAD_KEY.to_owned(),
                value: None,
            },
            &mut agent_meta,
        );
        assert!(!outcome.config_reload, "a tombstone must not ring it");

        // Wrong scope: some other consumer's key reuse must not ring it.
        let outcome = drive_meta_frame(
            FrameKind::MetadataChanged {
                scope: Scope::Terminal(tid(9)),
                key: CONFIG_RELOAD_KEY.to_owned(),
                value: Some(b"5678-99".to_vec()),
            },
            &mut agent_meta,
        );
        assert!(!outcome.config_reload, "non-Global scope must not ring it");
    }

    /// ADR-0040: malformed record bytes (bad JSON, empty name) must read
    /// as "no declared agent" — never a stored record, never a crash.
    #[test]
    fn agent_metadata_rejects_malformed_records() {
        use phux_protocol::wire::frame::{Scope, TERMINAL_AGENT_KEY};
        let pane = tid(1);
        let mut agent_meta = AgentMetaIndex::default();
        let outcome = drive_meta_frame(
            FrameKind::MetadataChanged {
                scope: Scope::Terminal(pane),
                key: TERMINAL_AGENT_KEY.to_owned(),
                value: Some(b"not json at all".to_vec()),
            },
            &mut agent_meta,
        );
        assert!(!outcome.agent_meta_changed);
        assert!(agent_meta.records.is_empty());
    }
}

#[cfg(test)]
mod session_name_tests {
    use super::focused_session_name;
    use phux_protocol::ids::{SessionId, TerminalId, WindowId};
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};

    fn snapshot_with(sessions: Vec<SessionInfo>, focused: SessionId) -> SessionSnapshot {
        SessionSnapshot::new(focused, WindowId::new(0), TerminalId::local(0))
            .with_sessions(sessions)
    }

    #[test]
    fn focused_session_name_resolves_the_matching_session() {
        // phux-17u: the widget reads the name of the focused session,
        // not the first session in the list.
        let snapshot = snapshot_with(
            vec![
                SessionInfo::new(SessionId::new(1), "work"),
                SessionInfo::new(SessionId::new(2), "play"),
            ],
            SessionId::new(2),
        );
        assert_eq!(focused_session_name(&snapshot), "play");
    }

    #[test]
    fn focused_session_name_is_empty_when_focus_is_absent() {
        // Degrade to an empty widget rather than panic if the focused
        // session somehow isn't in the list.
        let snapshot = snapshot_with(
            vec![SessionInfo::new(SessionId::new(1), "work")],
            SessionId::new(99),
        );
        assert_eq!(focused_session_name(&snapshot), "");
    }
}
