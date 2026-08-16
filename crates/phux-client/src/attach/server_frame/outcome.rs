//! `FrameOutcome` — the follow-up a handled frame asks of the driver —
//! and the small labeling helpers the handler logs and notices with.

use phux_protocol::ids::{ClientId, SessionId, TerminalId};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::info::SessionInfo;
use phux_protocol::{BootstrapId, StreamId};

use crate::attach::outcome::AttachEnd;
use crate::attach::paint::StatusBarPaint;
use crate::render::chrome::status_bar::Notice;

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
pub(in crate::attach) struct FrameOutcome {
    /// `true` ⇒ the loop should exit cleanly: either the server sent
    /// `DETACHED`, or a `TERMINAL_CLOSED` folded the last pane out of the
    /// layout and the consumer-owned detach policy (phux-4r1) decided to
    /// leave (nothing left to render or route input to).
    pub(in crate::attach) exit: bool,
    /// phux-i0e8.2.2: WHY the loop is exiting, when `exit` is `true`.
    /// `Some(LastPaneClosed { .. })` is set ONLY by the `TerminalClosed`
    /// arm when the fold emptied the workspace, carrying the dead pane's
    /// exit status so the CLI can explain the exit on the cooked terminal
    /// after teardown. `Some(Detached { reason })` is set by the `Detached`
    /// arm, carrying the server's stated reason (phux-l83x). `None` with
    /// `exit: true` is a detach with nothing to say about it; the driver
    /// folds it to a reason-less [`AttachEnd::Detached`].
    pub(in crate::attach) exit_reason: Option<AttachEnd>,
    /// `true` ⇒ ATTACHED just landed; the driver should emit
    /// `GET_METADATA` + `SUBSCRIBE_METADATA` for the layout key so
    /// other clients' mutations broadcast back to us (ADR-0019).
    pub(in crate::attach) subscribe_layout: bool,
    /// phux-k0cw: a layout broadcast for a session that is NOT this client's
    /// — `(session, Some(bytes))` for a value, `(session, None)` for a
    /// tombstone.
    ///
    /// This field is the whole point of the foreign-metadata guard. Before it
    /// existed, the layout arm matched the key FAMILY and adopted whatever it
    /// decoded as the local workspace, which was safe only while a client
    /// subscribed to exactly one layout key. The moment it watches peers, that
    /// same code path would let another session's window rearrangement replace
    /// your pane tree.
    pub(in crate::attach) foreign_layout: Option<(SessionId, Option<Vec<u8>>)>,
    /// phux-k0cw: a `phux.agent/v1` push for a Terminal this client does not
    /// hold a pane slot for.
    ///
    /// Routed out rather than folded into the local [`AgentMetaIndex`],
    /// because `sync_agent_meta_subscriptions` retains that index against the
    /// LOCAL pane set and would silently evict a foreign record on the next
    /// sweep.
    pub(in crate::attach) foreign_agent: Option<(TerminalId, Option<Vec<u8>>)>,
    /// phux-k0cw: an ADR-0035 `Asked` event for a Terminal outside this
    /// client's pane set — a peer agent is blocked on a human.
    pub(in crate::attach) foreign_attention: Option<TerminalId>,
    /// phux-k0cw: a `PaneSpawned` / `PaneClosed` for a Terminal this client
    /// does not hold, so the peer pane set (and its subscriptions) needs
    /// re-sweeping. This is what closes the enumerate-then-subscribe race
    /// without any wire change.
    pub(in crate::attach) foreign_pane_set_dirty: bool,
    /// `true` ⇒ the multi-pane composition needs a full repaint.
    ///
    /// Set both when the workspace was replaced by a server-side layout
    /// envelope AND when a bootstrap/attach READY reported damaged panes.
    /// It is a REPAINT signal, nothing more — do not use it to decide that a
    /// pending request was answered. See [`Self::layout_get_answered`].
    pub(in crate::attach) layout_replaced: bool,
    /// `true` ⇒ this frame WAS the `MetadataValue` answer to the driver's
    /// pending layout `GET_METADATA`, and the workspace has adopted it.
    ///
    /// Split out from [`Self::layout_replaced`] because that flag is also
    /// raised for pane damage during bootstrap. The driver keyed
    /// "the GET reply is single-use, clear the pending id" off the shared
    /// flag, so an `ATTACH_READY`/`BOOTSTRAP_READY` arriving first cleared the
    /// id and the real reply was then dropped as unsolicited — leaving the
    /// client on a single-leaf tree, so `next-pane` had nothing to cycle to
    /// and focus silently never moved.
    pub(in crate::attach) layout_get_answered: bool,
    /// Layout leaves newly discovered from peer metadata. The driver attaches
    /// each Terminal so its authoritative snapshot/output stream can populate
    /// a pane slot; this does not alter client-local focus.
    pub(in crate::attach) attach_panes: Vec<TerminalId>,
    /// phux-4li.12: `true` ⇒ the server-side frame mutated layout in
    /// a way the *local* client originated (split landed, kill folded);
    /// the driver should broadcast the new envelope via
    /// `SET_METADATA` so sibling clients reconcile.
    pub(in crate::attach) emit_set_metadata: bool,
    /// phux-tnh: `true` ⇒ a pane lifecycle event (close/spawn) changed
    /// surviving panes' dimensions. The driver must diff the new layout
    /// against the pre-frame rects and emit a `TERMINAL_RESIZE` per
    /// changed leaf so the server reflows each PTY (TIOCSWINSZ) — without
    /// this the survivor of a close keeps its old small winsize and the
    /// shell never redraws to fill the freed space. Set ONLY by the
    /// `TerminalClosed`/`TerminalSpawned` arms, not by the broader
    /// `layout_replaced` reconcile/broadcast paths (which already sized
    /// their panes and would otherwise thrash on attach).
    pub(in crate::attach) reflow_panes: bool,
    /// Exact cumulative `StateSync` acknowledgement emitted by the session kernel.
    pub(in crate::attach) ack: Option<(TerminalId, StreamId, BootstrapId, u64)>,
    /// The engine rejected a generation after emitting a typed resync status.
    ///
    /// The driver issues a fresh in-connection ATTACH while this outcome leaves
    /// the frozen published replica visible.
    pub(in crate::attach) resync_required: bool,
    /// Pull the next opaque native history page after READY or a prior page.
    pub(in crate::attach) history_request:
        Option<(TerminalId, StreamId, BootstrapId, bytes::Bytes, u32, u32)>,
    /// Exact terminal-engine response writes to forward on the ordered PTY lane.
    pub(in crate::attach) pty_writes: Vec<(TerminalId, Vec<u8>)>,
    /// phux-4li.20: `Some((sessions, focused))` ⇒ ATTACHED just landed
    /// and carried the server's full session graph. The driver caches
    /// it so the `<leader> a` session picker can list the other
    /// sessions without a follow-up request/response frame — the
    /// `ATTACHED` snapshot is already authoritative at attach time (SPEC
    /// §13). Set ONLY by the `Attached` arm.
    pub(in crate::attach) sessions: Option<(Vec<SessionInfo>, SessionId)>,
    /// ADR-0033: `Some(id)` ⇒ ATTACHED carried this client's own server-assigned
    /// `ClientId`. The driver caches it to tell "you have the wheel" from
    /// another client holding it when rendering the supervisory badge. Set ONLY
    /// by the `Attached` arm.
    pub(in crate::attach) own_client_id: Option<ClientId>,
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
    pub(in crate::attach) chrome_dirty: bool,
    /// ADR-0040: `true` ⇒ a `phux.agent/v1` record changed for some pane
    /// (a `GET_METADATA` reply or a `METADATA_CHANGED` broadcast). Window
    /// labels derive from it, so the driver refreshes the window chrome
    /// (tab strip + sidebar) and repaints. Set ONLY by the
    /// `MetadataValue` / `MetadataChanged` arms.
    pub(in crate::attach) agent_meta_changed: bool,
    /// phux-p4vp: per-pane working directories carried by the `ATTACHED`
    /// snapshot (`TerminalInfo::cwd`). The driver folds these into its
    /// pane-cwd index, from which the sidebar's branch line is derived
    /// client-side (see `crate::vcs`). Set ONLY by the `Attached` arm;
    /// empty otherwise.
    pub(in crate::attach) pane_cwds: Vec<(TerminalId, String)>,
    /// phux-foz.5: `true` ⇒ a subscribed `phux.config.reload/v1`
    /// doorbell rang (a `phux config reload` from some shell). The driver
    /// re-runs its layered config loader and swaps its config-derived
    /// state in place, exactly as for the `reload-config` action; on a
    /// failed re-read it keeps the previous config and surfaces the
    /// error. Set ONLY by the `MetadataChanged` arm; tombstones do not
    /// set it.
    pub(in crate::attach) config_reload: bool,
    /// phux-i0e8.2.1: transient status-bar notices raised by this frame,
    /// drained by the driver into the painter's newest-wins notice slot
    /// (`StatusBarPainter::set_notice`) right after the dispatch returns.
    /// Producers today: a focused-pane input-authority (`TerminalControl`)
    /// holder transition, and a degraded-federation push (an uncorrelated
    /// `ERROR { SATELLITE_UNREACHABLE }`). Empty on every other frame.
    pub(in crate::attach) notices: Vec<Notice>,
    /// A status-bar paint completed while handling this frame. Used to commit
    /// attach onboarding only after its notice reaches the render sink.
    pub(in crate::attach) status_bar_painted: StatusBarPaint,
}

/// Payload-free label for the inbound `FrameKind` — the `kind` field on
/// the per-frame dispatch span. Keeps the trace line small and free of
/// content bytes / session names; the heavy content frames additionally
/// record `terminal_id` / `seq` / `bytes`. `FrameKind` is large and
/// `#[non_exhaustive]`, so this covers the S->C arms this handler acts on
/// and folds the rest into `"other"`.
pub(super) const fn frame_kind_label(frame: &FrameKind) -> &'static str {
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
        FrameKind::Detached { .. } => "detached",
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
pub(super) fn input_authority_notice(holder: Option<ClientId>) -> String {
    holder.map_or_else(
        || "input: wheel released".to_owned(),
        |id| format!("input: c{} took the wheel", id.get()),
    )
}

/// phux-i0e8.2.2: user-facing name for a pane in a status-bar notice.
///
/// A local terminal reads `pane N`; a federation satellite's pane keeps
/// its host tag (`pane host/N`) so the notice does not alias two panes
/// with the same peer-local id.
pub(super) fn pane_label(id: &TerminalId) -> String {
    match id {
        TerminalId::Local { id } => format!("pane {id}"),
        TerminalId::Satellite { host, id } => format!("pane {host}/{id}"),
    }
}
