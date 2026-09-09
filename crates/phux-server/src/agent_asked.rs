//! The `agent.asked` ladder (ADR-0036): who gets to say a pane is waiting on
//! a human, and what a subscriber sees when several sources say it at once.
//!
//! Every source — an explicit hook report, the `phux-ask` title sentinel, a
//! future passive screen scrape — funnels through one [`AskedDetector`] per
//! server. The detector is the **single owner of what surfaces**: it ranks
//! sources ([`AskedSource::priority`]), coalesces a re-asserted question into
//! silence, and hands back an [`AskedTransition`] the caller broadcasts. A
//! producer's own edge filtering (the actor's title mirror, say) is a
//! transport optimization that keeps a channel quiet; it never decides
//! whether a client sees `AgentEvent::Asked`.
//!
//! Retraction is per-source ([`AskedDetector::retract`]): a source may take
//! back only what it asserted, so a sentinel that disappears cannot clear an
//! ask a hook is still standing behind.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private server module shared by sibling runtime/state modules"
)]

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use phux_core::ids::TerminalId;
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
    /// The `phux-ask` terminal-title sentinel — the interoperable v1 trigger
    /// any process that can emit OSC 0 / OSC 2 can drive. Explicit, so it
    /// outranks a scrape; unauthenticated and stateless, so it yields to a
    /// hook that owns the question's identity and lifecycle.
    Sentinel,
    /// An opt-in agent integration reporting through `REPORT_ASKED`. It owns
    /// identity and lifecycle, so it is authoritative.
    Hook,
    /// An `ask` record - or a `notification` record whose kind is
    /// `permission` or `elicitation` - on a live `AgentSession` child's
    /// stream (ADR-0103 decision 5).
    ///
    /// Top of the ladder for the same reason
    /// [`crate::agent_state::EvidenceSource::Stream`] is top of the state
    /// ladder: it is the agent describing itself over a sequenced channel it
    /// owns, so it carries the question's identity and lifecycle exactly as a
    /// hook does, and it cannot be silently lost the way an edge-triggered
    /// hook call can. Entered through
    /// [`crate::state::ServerState::report_stream_ask`]; retraction is
    /// per-source and unchanged, so a stream that goes quiet clears only what
    /// the stream itself asserted.
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
    states: HashMap<TerminalId, AskedState>,
}

impl AskedDetector {
    /// Record that `source` sees `terminal` waiting on a human, and say what
    /// that did to the pane's pending question.
    ///
    /// Three outcomes, and the order of the checks is the ladder:
    ///
    /// 1. A **lower-ranked** source cannot displace a higher-ranked one:
    ///    ignored, and the incumbent keeps the pane.
    /// 2. **The same question**, whoever is reporting it, is not a new event:
    ///    ignored. When the reporter outranks the incumbent the ownership
    ///    still transfers — an agent that drives both the title sentinel and
    ///    the hook is describing one ask, so the subscriber sees one event
    ///    and the hook (which can retract it) ends up owning it.
    /// 3. Anything else is a **different question** from a source entitled to
    ///    say so: it replaces the incumbent and is emitted.
    pub(crate) fn report(
        &mut self,
        terminal: TerminalId,
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

    /// Take back the pending question `source` asserted, returning it when
    /// there was one to take back.
    ///
    /// Per-source by design: a source may retract only a report it owns. A
    /// `phux-ask` title that disappears therefore clears a sentinel-owned ask
    /// but leaves a hook-owned one standing, which is what "the hook owns
    /// lifecycle" means in ADR-0036. Retraction emits nothing — there is no
    /// wire event for "the question went away" — it just stops the ledger
    /// from claiming a pane is blocked after its marker cleared, so the same
    /// question asked again is a new ask rather than a coalesced no-op.
    pub(crate) fn retract(
        &mut self,
        terminal: TerminalId,
        source: AskedSource,
    ) -> Option<AskedPayload> {
        match self.states.entry(terminal) {
            Entry::Occupied(slot) if slot.get().source == source => Some(slot.remove().payload),
            _ => None,
        }
    }

    pub(crate) fn clear_terminal(&mut self, terminal: TerminalId) -> Option<AskedPayload> {
        self.states.remove(&terminal).map(|state| state.payload)
    }

    #[cfg(test)]
    pub(crate) fn current(&self, terminal: TerminalId) -> Option<&AskedPayload> {
        self.states.get(&terminal).map(|state| &state.payload)
    }
}

#[cfg(test)]
mod tests {
    use phux_core::ids::TerminalId;

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
        let terminal = TerminalId::default();
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

    /// The sentinel's own dedupe lives here, not in its producer: a title the
    /// actor re-observes is the same ask and must not re-fire, while a title
    /// that changes is a new ask and must.
    #[test]
    fn a_re_asserted_sentinel_is_silent_and_a_changed_one_is_not() {
        let terminal = TerminalId::default();
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

    /// The double-up this whole change exists to prevent: an agent that
    /// drives BOTH the title sentinel and the hook for one question. The
    /// subscriber must see that ask once — and the hook must end up owning
    /// it, because only the hook can retract it.
    #[test]
    fn the_same_ask_from_sentinel_then_hook_fires_once() {
        let terminal = TerminalId::default();
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

    /// A sentinel that clears and comes back with the identical question is a
    /// second ask, not an echo of the first. Without the retract the
    /// detector's equality coalescing would swallow it and the pane would
    /// wait on a human nobody was told about.
    #[test]
    fn a_sentinel_that_clears_and_returns_fires_again() {
        let terminal = TerminalId::default();
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
        let terminal = TerminalId::default();
        let mut detector = AskedDetector::default();
        detector.report(terminal, AskedSource::Sentinel, payload("q1", "Deploy?"));
        assert_eq!(detector.retract(terminal, AskedSource::Scrape), None);
        assert!(detector.current(terminal).is_some());
    }

    #[test]
    fn hook_wins_over_scrape() {
        let terminal = TerminalId::default();
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
        let terminal = TerminalId::default();
        let mut detector = AskedDetector::default();
        detector.report(terminal, AskedSource::Hook, payload("hook", "Approve?"));
        assert!(detector.current(terminal).is_some());
        let cleared = detector.clear_terminal(terminal).unwrap();
        assert_eq!(cleared.id, "hook");
        assert!(detector.current(terminal).is_none());
    }

    /// The `Stream` rung (ADR-0103 decision 5) sits above `Hook`, and
    /// retraction stays per-source: a hook that takes its own question back
    /// cannot clear one the stream is standing behind.
    #[test]
    fn a_stream_ask_outranks_a_hook_ask() {
        let terminal = TerminalId::default();
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
