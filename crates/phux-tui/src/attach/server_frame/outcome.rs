//! `FrameOutcome` — the follow-up a handled frame asks of the driver —
//! and the small labeling helpers the handler logs and notices with.

use phux_protocol::ids::{ClientId, ResourceId, SessionId};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, WindowInfo};
use phux_protocol::{BootstrapId, StreamId};

use crate::attach::outcome::AttachEnd;
use crate::attach::paint::StatusBarPaint;
use crate::render::chrome::status_bar::Notice;

/// Outcome of processing one server frame: the async follow-ups the driver
/// owes, decided synchronously by [`handle_server_frame`].
#[allow(
    clippy::struct_excessive_bools,
    reason = "parallel server-frame outcome flags; refactor into bitset would obscure callers"
)]
#[derive(Debug, Clone, Default)]
pub(in crate::attach) struct FrameOutcome {
    /// Terminal whose authoritative output changed and still needs to become
    /// visible before an acknowledged-input ambiguity fence may clear.
    pub(in crate::attach) authoritative_damage: Vec<ResourceId>,
    /// Damaged Terminal this frame actually painted while it was focused.
    pub(in crate::attach) painted_output: Option<ResourceId>,
    /// End the loop: `DETACHED`, or the last pane closed (consumer-owned
    /// detach policy).
    pub(in crate::attach) exit: bool,
    /// Why the loop is exiting: `LastPaneClosed` with the dead pane's status,
    /// or `Detached` with the server's reason. `None` folds to a reason-less
    /// [`AttachEnd::Detached`].
    pub(in crate::attach) exit_reason: Option<AttachEnd>,
    /// ATTACHED landed: fetch and subscribe the layout key (ADR-0019).
    pub(in crate::attach) subscribe_layout: bool,
    /// A layout broadcast for another session (`None` value = tombstone),
    /// routed to the roster instead of replacing our pane tree.
    pub(in crate::attach) foreign_layout: Option<(SessionId, Option<Vec<u8>>)>,
    /// A `phux.agent/v1` push for a Terminal outside our pane set, kept out
    /// of the local index (whose sweep would evict it).
    pub(in crate::attach) foreign_agent: Option<(ResourceId, Option<Vec<u8>>)>,
    /// An ADR-0035 `Asked` event for a Terminal outside this
    /// client's pane set — a peer agent is blocked on a human.
    pub(in crate::attach) foreign_attention: Option<ResourceId>,
    /// ADR-0136: the asked flag cleared for a foreign Terminal.
    pub(in crate::attach) foreign_attention_clear: Option<ResourceId>,
    /// A spawn/close of a Terminal this client does not hold: re-sweep the
    /// peer pane set.
    pub(in crate::attach) foreign_pane_set_dirty: bool,
    /// Repaint the whole composition (layout replaced, or READY damage). A
    /// repaint signal only; see [`Self::layout_get_answered`].
    pub(in crate::attach) layout_replaced: bool,
    /// This frame answered the pending layout GET (adopted, or nothing
    /// stored); releases the write fence. Kept apart from `layout_replaced`,
    /// which READY damage also raises: sharing it once dropped the real reply.
    pub(in crate::attach) layout_get_answered: bool,
    /// Layout leaves discovered from peer metadata, for the driver to attach.
    pub(in crate::attach) attach_panes: Vec<ResourceId>,
    /// Windows/splits for satellite panes just spawned through the hub; each
    /// opens or applies only when its `ATTACH_RESOURCE` succeeds.
    pub(in crate::attach) adopt_spawned: Vec<crate::attach::actions::ParkedAdopt>,
    /// Spawned satellite panes whose attach was refused and that nothing
    /// references: best-effort `KILL_RESOURCE` through the hub.
    pub(in crate::attach) kill_orphans: Vec<ResourceId>,
    /// Bound spawned satellite panes stranded by an unreachable satellite,
    /// retried once via `KILL_RESOURCE_IF` when it answers again.
    pub(in crate::attach) unreachable_strays: Vec<phux_client::conditional_kill::BoundResource>,
    /// A locally originated layout change: broadcast via `SET_METADATA`.
    pub(in crate::attach) emit_set_metadata: bool,
    /// ADR-0105: a keep-empty session's last pane closed; delete the stored
    /// layout.
    pub(in crate::attach) clear_layout: bool,
    /// A close/spawn changed survivors' sizes: diff against the pre-frame
    /// rects and `RESIZE_TERMINAL` each changed leaf. Set only by those arms.
    pub(in crate::attach) reflow_panes: bool,
    /// ADR-0147: a floating overlay opened; `RESIZE_TERMINAL` it to its box.
    pub(in crate::attach) size_floating: bool,
    /// Exact cumulative `StateSync` acknowledgement emitted by the session kernel.
    pub(in crate::attach) ack: Option<(ResourceId, StreamId, BootstrapId, u64)>,
    /// The engine rejected a generation: re-ATTACH while the frozen replica
    /// stays visible.
    pub(in crate::attach) resync_required: bool,
    /// Pull the next opaque native history page after READY or a prior page.
    pub(in crate::attach) history_request:
        Option<(ResourceId, StreamId, BootstrapId, bytes::Bytes, u32, u32)>,
    /// A `DIRECTORY_LISTING` reply for the driver to match.
    pub(in crate::attach) directory_listing:
        Option<(u32, phux_protocol::wire::frame::DirectoryListingResult)>,
    /// Latest host path discovery reply, correlated by the driver.
    pub(in crate::attach) path_results: Option<(u32, phux_protocol::wire::frame::PathQueryResult)>,
    /// ATTACHED's session graph, for the session picker.
    pub(in crate::attach) sessions: Option<(Vec<SessionInfo>, SessionId)>,
    /// ATTACHED's windows and resources, so the Agents list can name panes in
    /// sessions with no persisted layout.
    pub(in crate::attach) inventory: Option<(Vec<WindowInfo>, Vec<ResourceInfo>)>,
    /// ADR-0033: this client's `ClientId` from ATTACHED.
    pub(in crate::attach) own_client_id: Option<ClientId>,
    /// Repaint the chrome though no grid changed: a lease/lifecycle/attention
    /// event, or applied bytes that moved the pane's OSC title.
    pub(in crate::attach) chrome_dirty: bool,
    /// ADR-0040: a `phux.agent/v1` record changed.
    pub(in crate::attach) agent_meta_changed: bool,
    /// The Terminal a local agent record applied to, even when identical, for
    /// the review index.
    pub(in crate::attach) agent_meta_terminal: Option<ResourceId>,
    /// ATTACHED's per-pane cwds, for the sidebar branch line.
    pub(in crate::attach) pane_cwds: Vec<(ResourceId, String)>,
    /// The config-reload doorbell rang.
    pub(in crate::attach) config_reload: bool,
    /// A `phux.session.name/v1` rename `(current, new)`.
    pub(in crate::attach) session_rename: Option<(String, String)>,
    /// Transient status-bar notices raised by this frame.
    pub(in crate::attach) notices: Vec<Notice>,
    /// A status-bar paint completed while handling this frame. Used to commit
    /// attach onboarding only after its notice reaches the render sink.
    pub(in crate::attach) status_bar_painted: StatusBarPaint,
}

/// Payload-free label for the dispatch span's `kind` field.
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
        FrameKind::ResourceOutput { .. } => "terminal_output",
        FrameKind::Detached { .. } => "detached",
        FrameKind::Bell { .. } => "bell",
        FrameKind::MetadataValue { .. } => "metadata_value",
        FrameKind::MetadataChanged { .. } => "metadata_changed",
        FrameKind::DirectoryListing { .. } => "directory_listing",
        FrameKind::PathResults { .. } => "path_results",
        FrameKind::ResourceSpawned { .. } => "terminal_spawned",
        FrameKind::ResourceClosed { .. } => "terminal_closed",
        _ => "other",
    }
}

/// Text for the focused pane's input-authority notice (same `c<N>` spelling
/// as the ADR-0033 badge).
pub(super) fn input_authority_notice(holder: Option<ClientId>) -> String {
    holder.map_or_else(
        || "input: wheel released".to_owned(),
        |id| format!("input: c{} took the wheel", id.get()),
    )
}

/// A pane's name in a notice: `pane N`, or `pane host/N` for a satellite.
pub(super) fn pane_label(id: &ResourceId) -> String {
    match id {
        ResourceId::Local { id } => format!("pane {id}"),
        ResourceId::Satellite { host, id } => format!("pane {host}/{id}"),
    }
}
