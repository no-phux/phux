//! Connection-lifetime agent review state (phux-deya).
//!
//! `PaneSlot.seen` dies with [`super::loop_state::SessionLoop`]. Review is
//! per-viewer and per-identity, so it lives next to orphan bookkeeping in
//! the outer attach loop and is keyed by [`ResourceId`]. Local and foreign
//! GET/broadcast folds, and `AgentSession` stream snapshots, all share this
//! index. Identical observations do not re-arm; a switch-drain frame that
//! cannot be interpreted marks a gap because equality cannot recover a
//! missed done-working-done cycle without a revision.

use std::collections::HashMap;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, Scope};

use crate::attach::agent_rows::AgentSessionRows;
use phux_client::agent_meta::{
    AgentMetaState, AgentRecord, RESOURCE_AGENT_KEY, parse_agent_record,
};

/// What the chrome currently shows for one agent identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Observation {
    record: Option<AgentRecord>,
    stream: Vec<(ResourceId, AgentMetaState)>,
}

#[derive(Clone, Debug, Default)]
struct ReviewEntry {
    observation: Observation,
    seen: bool,
    /// Set when a switch-drain frame proved a change we could not fold.
    gap: bool,
}

/// Per-connection review index: latest observation plus whether this client
/// has visited that observation.
#[derive(Debug, Default)]
pub(in crate::attach) struct ReviewIndex {
    entries: HashMap<ResourceId, ReviewEntry>,
    /// `AgentSession` resource → parent pane, so a drain `ResourceOutput`
    /// can mark a gap on the identity the chrome actually reviews.
    stream_parents: HashMap<ResourceId, ResourceId>,
}

impl ReviewIndex {
    pub const fn new() -> Self {
        Self {
            entries: HashMap::new(),
            stream_parents: HashMap::new(),
        }
    }

    /// Whether this identity is currently reviewed. Unknown identities are
    /// unreviewed — a pane we have never observed has never been visited.
    pub(in crate::attach) fn is_seen(&self, id: &ResourceId) -> bool {
        self.entries.get(id).is_some_and(|entry| entry.seen)
    }

    /// Review status when this index has an entry, otherwise `fallback`
    /// (the session-local `PaneSlot.seen` cache unit tests still write).
    pub(in crate::attach) fn seen_or(&self, id: &ResourceId, fallback: bool) -> bool {
        self.entries.get(id).map_or(fallback, |entry| entry.seen)
    }

    /// Mark `id` reviewed because the user is looking at it.
    ///
    /// Returns `true` on the flip (`false` → `true`) so the caller can
    /// schedule a chrome repaint exactly once.
    pub(in crate::attach) fn mark_seen(&mut self, id: &ResourceId) -> bool {
        let entry = self.entries.entry(id.clone()).or_default();
        let flipped = !entry.seen;
        entry.seen = true;
        flipped
    }

    /// Confirmed death or metadata deletion. Locality changes (local ↔
    /// foreign) must not call this: the identity is the same pane.
    pub(in crate::attach) fn forget(&mut self, id: &ResourceId) -> bool {
        let mut changed = self.entries.remove(id).is_some();
        if let Some(pane) = self.stream_parents.remove(id) {
            if let Some(entry) = self.entries.get_mut(&pane) {
                let before = entry.observation.clone();
                entry.observation.stream.retain(|(sid, _)| sid != id);
                if entry.observation != before {
                    entry.seen = false;
                    changed = true;
                }
            }
            changed = true;
        }
        self.stream_parents.retain(|_, pane| pane != id);
        changed
    }

    /// Fold a GET or broadcast. `record = None` is a tombstone.
    ///
    /// Returns whether the stored observation or review status changed.
    pub(in crate::attach) fn observe_record(
        &mut self,
        id: &ResourceId,
        record: Option<&AgentRecord>,
        focused: Option<&ResourceId>,
    ) -> bool {
        let Some(record) = record else {
            return self.forget(id);
        };
        self.fold(id, focused, |entry| {
            entry.observation.record = Some(record.clone());
        })
    }

    /// Fold the `AgentSession` rows currently bound to `pane`.
    pub(in crate::attach) fn observe_stream(
        &mut self,
        pane: &ResourceId,
        sessions: &[(ResourceId, AgentMetaState)],
        focused: Option<&ResourceId>,
    ) -> bool {
        for (session, _) in sessions {
            self.stream_parents.insert(session.clone(), pane.clone());
        }
        self.stream_parents.retain(|session, parent| {
            parent != pane || sessions.iter().any(|(id, _)| id == session)
        });
        self.fold(pane, focused, |entry| {
            let mut stream = sessions.to_vec();
            stream.sort_by(|a, b| a.0.cmp(&b.0));
            entry.observation.stream = stream;
        })
    }

    /// Project every live stream onto its parent pane. Absence from `rows`
    /// is not retraction: that would treat a session-loop rebuild as death.
    pub(in crate::attach) fn observe_streams(
        &mut self,
        rows: &AgentSessionRows,
        focused: Option<&ResourceId>,
    ) -> bool {
        let mut changed = false;
        for (pane, sessions) in rows {
            let snap: Vec<_> = sessions.iter().map(|s| (s.id.clone(), s.state)).collect();
            changed |= self.observe_stream(pane, &snap, focused);
        }
        changed
    }

    /// One frame the switch drained before `DETACHED`. Metadata we can parse
    /// folds; an `AgentSession` output we cannot parse marks a gap so a later
    /// identical snapshot cannot hide a missed cycle.
    pub(in crate::attach) fn observe_switch_drain(&mut self, frame: &FrameKind) {
        match frame {
            FrameKind::MetadataChanged {
                scope: Scope::Resource(id),
                key,
                value,
                ..
            } if key == RESOURCE_AGENT_KEY => {
                let parsed = value.as_deref().and_then(parse_agent_record);
                self.observe_record(id, parsed.as_ref(), None);
            }
            FrameKind::ResourceClosed { terminal_id, .. } => {
                self.forget(terminal_id);
            }
            FrameKind::ResourceOutput { terminal_id, .. } => {
                if let Some(pane) = self.stream_parents.get(terminal_id).cloned()
                    && let Some(entry) = self.entries.get_mut(&pane)
                {
                    entry.gap = true;
                }
            }
            _ => {}
        }
    }

    fn fold(
        &mut self,
        id: &ResourceId,
        focused: Option<&ResourceId>,
        update: impl FnOnce(&mut ReviewEntry),
    ) -> bool {
        let focused_here = focused == Some(id);
        let entry = self.entries.entry(id.clone()).or_default();
        let before = entry.observation.clone();
        let gapped = std::mem::take(&mut entry.gap);
        update(entry);
        let same = !gapped && before == entry.observation;
        if focused_here {
            let flipped = !entry.seen;
            entry.seen = true;
            flipped || !same
        } else if same {
            false
        } else if entry.seen && before == Observation::default() {
            // First observation of a pane the user already visited.
            false
        } else {
            entry.seen = false;
            true
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn rec(name: &str, state: AgentMetaState) -> AgentRecord {
        AgentRecord {
            name: name.to_owned(),
            state,
            ..AgentRecord::default()
        }
    }

    fn pane() -> ResourceId {
        ResourceId::local(10)
    }

    fn session() -> ResourceId {
        ResourceId::local(99)
    }

    #[test]
    fn identical_record_keeps_a_reviewed_done() {
        let id = pane();
        let done = rec("reviewer", AgentMetaState::Done);
        let mut review = ReviewIndex::default();
        assert!(review.observe_record(&id, Some(&done), Some(&id)));
        assert!(review.is_seen(&id));
        assert!(
            !review.observe_record(&id, Some(&done), None),
            "identical GET must not re-arm"
        );
        assert!(review.is_seen(&id));
    }

    #[test]
    fn a_genuine_change_rearms_when_unfocused() {
        let id = pane();
        let mut review = ReviewIndex::default();
        review.observe_record(&id, Some(&rec("reviewer", AgentMetaState::Done)), Some(&id));
        assert!(review.observe_record(&id, Some(&rec("reviewer", AgentMetaState::Working)), None));
        assert!(!review.is_seen(&id));
        review.observe_record(&id, Some(&rec("reviewer", AgentMetaState::Done)), None);
        assert!(!review.is_seen(&id), "a new completion is unread");
    }

    #[test]
    fn a_focused_change_stays_reviewed() {
        let id = pane();
        let mut review = ReviewIndex::default();
        review.observe_record(
            &id,
            Some(&rec("reviewer", AgentMetaState::Working)),
            Some(&id),
        );
        assert!(review.observe_record(
            &id,
            Some(&rec("reviewer", AgentMetaState::Done)),
            Some(&id)
        ));
        assert!(review.is_seen(&id));
    }

    #[test]
    fn tombstone_and_death_clear_identity() {
        let id = pane();
        let mut review = ReviewIndex::default();
        review.observe_record(&id, Some(&rec("reviewer", AgentMetaState::Done)), Some(&id));
        assert!(review.observe_record(&id, None, None));
        assert!(!review.is_seen(&id));
        review.observe_record(&id, Some(&rec("reviewer", AgentMetaState::Done)), Some(&id));
        assert!(review.forget(&id));
        assert!(!review.is_seen(&id));
        assert!(
            !review.forget(&id),
            "a second forget is not a review change"
        );
    }

    #[test]
    fn locality_does_not_prune() {
        let id = pane();
        let mut review = ReviewIndex::default();
        review.observe_record(&id, Some(&rec("reviewer", AgentMetaState::Done)), Some(&id));
        // Switching sessions rebuilds the local pane set; the index must
        // keep the identity so a later foreign or local GET can fold equal.
        assert!(review.is_seen(&id));
        assert!(!review.observe_record(&id, Some(&rec("reviewer", AgentMetaState::Done)), None));
        assert!(review.is_seen(&id));
    }

    #[test]
    fn stream_completion_invalidates_an_unfocused_pane() {
        let id = pane();
        let sid = session();
        let mut review = ReviewIndex::default();
        review.observe_stream(&id, &[(sid.clone(), AgentMetaState::Working)], Some(&id));
        assert!(review.is_seen(&id));
        assert!(review.observe_stream(&id, &[(sid, AgentMetaState::Done)], None));
        assert!(!review.is_seen(&id));
    }

    #[test]
    fn identical_stream_keeps_review() {
        let id = pane();
        let sid = session();
        let mut review = ReviewIndex::default();
        review.observe_stream(&id, &[(sid.clone(), AgentMetaState::Done)], Some(&id));
        assert!(!review.observe_stream(&id, &[(sid, AgentMetaState::Done)], None));
        assert!(review.is_seen(&id));
    }

    #[test]
    fn drain_metadata_change_rearms_before_the_identical_get() {
        let id = pane();
        let done = rec("reviewer", AgentMetaState::Done);
        let mut review = ReviewIndex::default();
        review.observe_record(&id, Some(&done), Some(&id));
        review.observe_switch_drain(&FrameKind::MetadataChanged {
            scope: Scope::Resource(id.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: Some(rec("reviewer", AgentMetaState::Working).encode()),
            actor: None,
        });
        assert!(!review.is_seen(&id));
        review.observe_record(&id, Some(&done), None);
        assert!(!review.is_seen(&id));
    }

    #[test]
    fn drain_stream_output_marks_a_gap_equality_cannot_heal() {
        let id = pane();
        let sid = session();
        let mut review = ReviewIndex::default();
        review.observe_stream(&id, &[(sid.clone(), AgentMetaState::Done)], Some(&id));
        review.observe_switch_drain(&FrameKind::ResourceOutput {
            terminal_id: sid.clone(),
            stream_id: phux_protocol::StreamId::new(1).expect("stream"),
            bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
            seq: 1,
            bytes: bytes::Bytes::new(),
        });
        assert!(
            review.observe_stream(&id, &[(sid, AgentMetaState::Done)], None),
            "a missed cycle must re-arm even when the snapshot matches"
        );
        assert!(!review.is_seen(&id));
    }

    #[test]
    fn drain_close_forgets_without_a_locality_hint() {
        let id = pane();
        let mut review = ReviewIndex::default();
        review.observe_record(&id, Some(&rec("reviewer", AgentMetaState::Done)), Some(&id));
        review.observe_switch_drain(&FrameKind::ResourceClosed {
            terminal_id: id.clone(),
            exit_status: None,
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
            signal: None,
        });
        assert!(!review.is_seen(&id));
    }
}
