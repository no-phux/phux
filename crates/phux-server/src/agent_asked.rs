//! The `agent.asked` ladder (ADR-0036): which source may say a pane waits on
//! a human, and what subscribers see.
//!
//! Every source funnels through one [`AskedDetector`], which ranks sources
//! ([`AskedSource::priority`]), coalesces repeats, and returns the
//! [`AskedTransition`] to broadcast. Retraction is per-source: a source
//! takes back only what it asserted.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private server module shared by sibling runtime/state modules"
)]

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use phux_core::ids::ResourceId;
use phux_protocol::wire::frame::AgentEvent;

/// Where a pending-question report came from, ordered by authority
/// (ADR-0036 §Decision).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AskedSource {
    /// Passive screen evidence. Advisory only: it fills the gap when nothing
    /// explicit is available and must never outrank an explicit source.
    #[allow(
        dead_code,
        reason = "passive scrape source lands after the detector core"
    )]
    Scrape,
    /// The `phux-ask` title sentinel: explicit, so it outranks a scrape, but
    /// yields to a hook that owns the question's lifecycle.
    Sentinel,
    /// An opt-in agent integration reporting through `REPORT_ASKED`. It owns
    /// identity and lifecycle, so it is authoritative.
    Hook,
    /// A live `AgentSession` child's `ask` (or permission/elicitation
    /// notification) record (ADR-0103 §5): top of the ladder, like
    /// [`crate::agent_state::EvidenceSource::Stream`].
    #[allow(
        dead_code,
        reason = "the AgentSession stream producer lands with the engine; the rung is defined here so the ladder is complete and ordered when it does"
    )]
    Stream,
}

impl AskedSource {
    /// Rank within the ladder: higher wins. A report from a lower-ranked
    /// source is ignored while a higher-ranked one holds the pane.
    const fn priority(self) -> u8 {
        match self {
            Self::Scrape => 0,
            Self::Sentinel => 1,
            Self::Hook => 2,
            Self::Stream => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AskedPayload {
    pub(crate) id: String,
    pub(crate) question: String,
    pub(crate) suggestions: Vec<String>,
    pub(crate) elapsed_seconds: Option<u64>,
}

impl AskedPayload {
    pub(crate) fn into_event(self) -> AgentEvent {
        AgentEvent::Asked {
            id: self.id,
            question: self.question,
            suggestions: self.suggestions,
            elapsed_seconds: self.elapsed_seconds,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AskedTransition {
    Entered(AskedPayload),
    Updated(AskedPayload),
    Ignored,
}

impl AskedTransition {
    pub(crate) fn emit_payload(self) -> Option<AskedPayload> {
        match self {
            Self::Entered(payload) | Self::Updated(payload) => Some(payload),
            Self::Ignored => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AskedState {
    source: AskedSource,
    payload: AskedPayload,
}

#[derive(Debug, Default)]
pub(crate) struct AskedDetector {
    states: HashMap<ResourceId, AskedState>,
}

impl AskedDetector {
    /// Record that `source` sees `terminal` waiting on a human.
    ///
    /// A lower-ranked source cannot displace a higher one; the same question
    /// is not a new event (but ownership moves to a higher-ranked reporter,
    /// so a hook can later retract it); anything else replaces and emits.
    pub(crate) fn report(
        &mut self,
        terminal: ResourceId,
        source: AskedSource,
        payload: AskedPayload,
    ) -> AskedTransition {
        match self.states.entry(terminal) {
            Entry::Occupied(mut slot) => {
                let existing = slot.get_mut();
                if existing.source.priority() > source.priority() {
                    return AskedTransition::Ignored;
                }
                existing.source = source;
                if existing.payload == payload {
                    return AskedTransition::Ignored;
                }
                existing.payload = payload.clone();
                AskedTransition::Updated(payload)
            }
            Entry::Vacant(slot) => {
                slot.insert(AskedState {
                    source,
                    payload: payload.clone(),
                });
                AskedTransition::Entered(payload)
            }
        }
    }

    /// Take back `source`'s own question, if any. Emits nothing, but a
    /// re-asked identical question is then a new ask.
    pub(crate) fn retract(
        &mut self,
        terminal: ResourceId,
        source: AskedSource,
    ) -> Option<AskedPayload> {
        match self.states.entry(terminal) {
            Entry::Occupied(slot) if slot.get().source == source => Some(slot.remove().payload),
            _ => None,
        }
    }

    pub(crate) fn clear_terminal(&mut self, terminal: ResourceId) -> Option<AskedPayload> {
        self.states.remove(&terminal).map(|state| state.payload)
    }

    /// Whether any source holds a question on `terminal` (the
    /// `phux.agent.asked/v1` flag, ADR-0136).
    pub(crate) fn is_pending(&self, terminal: ResourceId) -> bool {
        self.states.contains_key(&terminal)
    }

    #[cfg(test)]
    pub(crate) fn current(&self, terminal: ResourceId) -> Option<&AskedPayload> {
        self.states.get(&terminal).map(|state| &state.payload)
    }
}

#[cfg(test)]
mod tests {
    use phux_core::ids::ResourceId;

    use super::{AskedDetector, AskedPayload, AskedSource, AskedTransition};

    fn payload(id: &str, question: &str) -> AskedPayload {
        AskedPayload {
            id: id.to_owned(),
            question: question.to_owned(),
            suggestions: vec!["yes".to_owned(), "no".to_owned()],
            elapsed_seconds: None,
        }
    }

    /// The full ladder in one pass: each rung displaces the one below it and
    /// none of them can be displaced from below (ADR-0036 §Decision).
    #[test]
    fn each_rung_outranks_the_one_below_it() {
        let terminal = ResourceId::default();
        let mut detector = AskedDetector::default();
        assert!(matches!(
            detector.report(terminal, AskedSource::Scrape, payload("s", "Continue?")),
            AskedTransition::Entered(_)
        ));
        assert!(
            matches!(
                detector.report(terminal, AskedSource::Sentinel, payload("t", "Deploy?")),
                AskedTransition::Updated(_)
            ),
            "an explicit sentinel outranks passive screen evidence"
        );
        assert_eq!(
            detector.report(terminal, AskedSource::Scrape, payload("s2", "Continue?")),
            AskedTransition::Ignored,
            "and the scrape cannot take the pane back"
        );
        assert!(
            matches!(
                detector.report(terminal, AskedSource::Hook, payload("h", "Approve?")),
                AskedTransition::Updated(_)
            ),
            "a hook outranks the sentinel"
        );
        assert_eq!(
            detector.report(terminal, AskedSource::Sentinel, payload("t2", "Deploy?")),
            AskedTransition::Ignored,
            "and the sentinel cannot take the pane back"
        );
        assert_eq!(detector.current(terminal).unwrap().id, "h");
    }

    /// A re-observed title does not re-fire; a changed one does.
    #[test]
    fn a_re_asserted_sentinel_is_silent_and_a_changed_one_is_not() {
        let terminal = ResourceId::default();
        let mut detector = AskedDetector::default();
        assert!(matches!(
            detector.report(terminal, AskedSource::Sentinel, payload("q1", "Deploy?")),
            AskedTransition::Entered(_)
        ));
        assert_eq!(
            detector.report(terminal, AskedSource::Sentinel, payload("q1", "Deploy?")),
            AskedTransition::Ignored,
            "the identical marker again is one ask, not two"
        );
        assert!(matches!(
            detector.report(terminal, AskedSource::Sentinel, payload("q2", "Ship it?")),
            AskedTransition::Updated(_)
        ));
        assert_eq!(detector.current(terminal).unwrap().id, "q2");
    }

    /// Sentinel and hook for one question: one event, owned by the hook.
    #[test]
    fn the_same_ask_from_sentinel_then_hook_fires_once() {
        let terminal = ResourceId::default();
        let mut detector = AskedDetector::default();
        assert!(matches!(
            detector.report(terminal, AskedSource::Sentinel, payload("q1", "Deploy?")),
            AskedTransition::Entered(_)
        ));
        assert_eq!(
            detector.report(terminal, AskedSource::Hook, payload("q1", "Deploy?")),
            AskedTransition::Ignored,
            "the hook is vouching for the ask already on the wire, not a new one",
        );
        assert_eq!(
            detector.retract(terminal, AskedSource::Sentinel),
            None,
            "the sentinel no longer owns it, so its marker clearing must not \
             drop a question the hook is standing behind",
        );
        assert!(detector.current(terminal).is_some());
        assert!(
            detector.retract(terminal, AskedSource::Hook).is_some(),
            "the hook owns it and can take it back"
        );
    }

    /// A sentinel that clears and returns with the same question fires again.
    #[test]
    fn a_sentinel_that_clears_and_returns_fires_again() {
        let terminal = ResourceId::default();
        let mut detector = AskedDetector::default();
        assert!(matches!(
            detector.report(terminal, AskedSource::Sentinel, payload("q1", "Deploy?")),
            AskedTransition::Entered(_)
        ));
        assert_eq!(
            detector
                .retract(terminal, AskedSource::Sentinel)
                .unwrap()
                .id,
            "q1"
        );
        assert!(detector.current(terminal).is_none());
        assert!(matches!(
            detector.report(terminal, AskedSource::Sentinel, payload("q1", "Deploy?")),
            AskedTransition::Entered(_)
        ));
    }

    /// Retraction is per-source in both directions: a lower rung cannot
    /// silence a pane it does not own either.
    #[test]
    fn a_scrape_cannot_retract_a_sentinels_ask() {
        let terminal = ResourceId::default();
        let mut detector = AskedDetector::default();
        detector.report(terminal, AskedSource::Sentinel, payload("q1", "Deploy?"));
        assert_eq!(detector.retract(terminal, AskedSource::Scrape), None);
        assert!(detector.current(terminal).is_some());
    }

    #[test]
    fn hook_wins_over_scrape() {
        let terminal = ResourceId::default();
        let mut detector = AskedDetector::default();
        assert!(matches!(
            detector.report(
                terminal,
                AskedSource::Scrape,
                payload("scrape", "Continue?")
            ),
            AskedTransition::Entered(_)
        ));
        assert!(matches!(
            detector.report(terminal, AskedSource::Hook, payload("hook", "Approve?")),
            AskedTransition::Updated(_)
        ));
        assert_eq!(detector.current(terminal).unwrap().id, "hook");
        assert_eq!(
            detector.report(
                terminal,
                AskedSource::Scrape,
                payload("scrape-2", "Still waiting?")
            ),
            AskedTransition::Ignored
        );
        assert_eq!(detector.current(terminal).unwrap().id, "hook");
    }

    #[test]
    fn clear_terminal_drops_pending_ask() {
        let terminal = ResourceId::default();
        let mut detector = AskedDetector::default();
        detector.report(terminal, AskedSource::Hook, payload("hook", "Approve?"));
        assert!(detector.current(terminal).is_some());
        let cleared = detector.clear_terminal(terminal).unwrap();
        assert_eq!(cleared.id, "hook");
        assert!(detector.current(terminal).is_none());
    }

    /// `Stream` outranks `Hook`; a hook cannot retract the stream's question.
    #[test]
    fn a_stream_ask_outranks_a_hook_ask() {
        let terminal = ResourceId::default();
        let mut detector = AskedDetector::default();

        assert!(matches!(
            detector.report(terminal, AskedSource::Hook, payload("h", "Approve?")),
            AskedTransition::Entered(_)
        ));
        assert!(
            matches!(
                detector.report(terminal, AskedSource::Stream, payload("s", "Deploy?")),
                AskedTransition::Updated(_)
            ),
            "a record on the agent's own stream outranks its hook call",
        );
        assert_eq!(
            detector.report(terminal, AskedSource::Hook, payload("h2", "Approve?")),
            AskedTransition::Ignored,
            "and cannot be displaced from below",
        );
        assert_eq!(
            detector.retract(terminal, AskedSource::Hook),
            None,
            "retraction is per-source: the hook no longer owns the question",
        );
        assert_eq!(
            detector
                .retract(terminal, AskedSource::Stream)
                .map(|p| p.id),
            Some("s".to_owned()),
            "the stream retracts what the stream asserted",
        );
    }

    /// The full ask ladder in rank order, so a rung inserted in the wrong
    /// place is a failure here rather than a mystery in the sidebar.
    #[test]
    fn the_ask_ladder_runs_scrape_sentinel_hook_stream() {
        let ladder = [
            AskedSource::Scrape,
            AskedSource::Sentinel,
            AskedSource::Hook,
            AskedSource::Stream,
        ];
        for pair in ladder.windows(2) {
            assert!(
                pair[1].priority() > pair[0].priority(),
                "{:?} must outrank {:?}",
                pair[1],
                pair[0],
            );
        }
    }
}
