//! The driver-held index of `phux.agent/v1` records and the attention
//! ladder's change-clock re-arm.

use std::collections::HashMap;

use phux_protocol::ids::ResourceId;

use crate::attach::pane_state::PaneSlot;
use phux_client::agent_meta::{AgentRecord, parse_agent_record};

/// ADR-0040: the driver-held index of `phux.agent/v1` records: `records` for
/// the chrome, `pending` GET correlation, `subscribed` for an idempotent
/// subscription sweep.
#[derive(Debug, Default)]
pub(in crate::attach) struct AgentMetaIndex {
    /// Terminal → its decoded agent record (absent = no declared agent).
    pub(in crate::attach) records: HashMap<ResourceId, AgentRecord>,
    /// In-flight `GET_METADATA` request id → the Terminal it targets.
    pub(in crate::attach) pending: HashMap<u32, ResourceId>,
    /// In-flight `GET_METADATA` of `phux.agent.asked/v1`. Separate from
    /// [`Self::pending`]: the value is the byte `1`, which is not an agent
    /// record and must not clear one (ADR-0136).
    pub(in crate::attach) asked_pending: HashMap<u32, ResourceId>,
    /// In-flight `GET_METADATA` of `phux.pane-occupant/v1`, whose
    /// foreground lands on the pane's slot.
    pub(in crate::attach) occupant_pending: HashMap<u32, ResourceId>,
    /// Terminals with a live `SUBSCRIBE_METADATA` on the agent key.
    pub(in crate::attach) subscribed: std::collections::HashSet<ResourceId>,
    /// Terminal to when its record last changed: the attention ladder's
    /// tiebreak. Kept out of `AgentEntry`, the sidebar painter's cache key,
    /// where a timestamp would repaint the strip every frame.
    pub(in crate::attach) change_at: HashMap<ResourceId, std::time::Instant>,
}

impl AgentMetaIndex {
    /// Apply a metadata value for `terminal` (`None` bytes = tombstone);
    /// `true` when the record actually changed. A change stamps
    /// [`Self::change_at`]; a tombstone clears it.
    pub(super) fn apply(&mut self, terminal: &ResourceId, bytes: Option<&[u8]>) -> bool {
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

/// Fold an agent-record change into the attention bookkeeping: a new state
/// on an unfocused pane is unseen (it climbs the ladder); one on the focused
/// pane is seen by definition.
pub(super) fn note_agent_change(
    panes: &mut HashMap<ResourceId, PaneSlot>,
    focused_resource: Option<&ResourceId>,
    terminal: &ResourceId,
) {
    if focused_resource == Some(terminal) {
        return;
    }
    if let Some(slot) = panes.get_mut(terminal) {
        slot.seen = false;
    }
}
