//! The driver-held index of `phux.agent/v1` records and the attention
//! ladder's change-clock re-arm.

use std::collections::HashMap;

use phux_protocol::ids::TerminalId;

use crate::agent_meta::{AgentRecord, parse_agent_record};
use crate::attach::pane_state::PaneSlot;

/// ADR-0040 (`phux-3ert`): the driver-held index of `phux.agent/v1` records.
///
/// `records` is what the window chrome reads (structured agent labels for
/// the sidebar/tab strip); `pending` correlates in-flight `GET_METADATA`
/// request ids to the Terminal they asked about; `subscribed` tracks which
/// Terminals already have a live `SUBSCRIBE_METADATA` so the driver's
/// subscription sweep is idempotent.
#[derive(Debug, Default)]
pub(in crate::attach) struct AgentMetaIndex {
    /// Terminal → its decoded agent record (absent = no declared agent).
    pub(in crate::attach) records: HashMap<TerminalId, AgentRecord>,
    /// In-flight `GET_METADATA` request id → the Terminal it targets.
    pub(in crate::attach) pending: HashMap<u32, TerminalId>,
    /// Terminals with a live `SUBSCRIBE_METADATA` on the agent key.
    pub(in crate::attach) subscribed: std::collections::HashSet<TerminalId>,
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
    pub(in crate::attach) change_at: HashMap<TerminalId, std::time::Instant>,
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
    pub(super) fn apply(&mut self, terminal: &TerminalId, bytes: Option<&[u8]>) -> bool {
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
pub(super) fn note_agent_change(
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
