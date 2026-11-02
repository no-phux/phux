//! Constructing the detector, injecting hook and stream evidence, and its cadence.

use std::rc::Rc;
use std::time::{Duration, Instant};

use super::rules::RuleSet;

#[cfg(test)]
use super::IdentitySource;
use super::{
    AgentDetector, AgentReport, Cadence, DetectedState, TICK_CONFIRMING, TICK_IDENTIFIED,
    TICK_UNIDENTIFIED, live_session,
};

impl AgentDetector {
    /// Inject hook evidence. The actor marks the grid dirty afterwards, so the
    /// next tick may supersede it with screen evidence.
    /// detector tick re-evaluates screen evidence and may supersede the hook.
    pub(crate) fn report_hook_state(
        &mut self,
        state: DetectedState,
        now: Instant,
    ) -> Option<AgentReport> {
        if !phux_resource::agent_state::EvidenceSource::Hook.outranks(self.published_source) {
            // A live stream carries the same facts in order; keep the hook
            // pending for when the stream retracts.
            self.pending_hook = Some((state, now));
            return None;
        }
        let Some(kind) = self.identified.clone() else {
            self.pending_hook = Some((state, now));
            return None;
        };
        let report = AgentReport {
            name: self.manifest_name(&kind),
            kind,
            state,
        };
        self.pending_idle = None;
        self.cadence = Cadence::Identified;
        self.current = Some(state);
        self.published_source = phux_resource::agent_state::EvidenceSource::Hook;
        if self.published.as_ref() == Some(&report) {
            return None;
        }
        self.published = Some(report.clone());
        Some(report)
    }

    /// Publish an `AgentSession` child's state at the `Stream` rank
    /// (ADR-0103 §5). `None` retracts: the rank drops and the next ordinary
    /// tick derives what is true. Needs an identified pane; until then the
    /// evidence waits in the pending slot.
    pub(crate) fn report_stream_state(
        &mut self,
        state: Option<DetectedState>,
        now: Instant,
    ) -> Option<AgentReport> {
        self.stream_state = state;
        let Some(state) = state else {
            // Withdrawing a claim publishes nothing by itself.
            self.published_source = phux_resource::agent_state::EvidenceSource::Screen;
            self.pending_idle = None;
            return None;
        };
        self.published_source = phux_resource::agent_state::EvidenceSource::Stream;
        let Some(kind) = self.identified.clone() else {
            self.pending_hook = Some((state, now));
            return None;
        };
        let report = AgentReport {
            name: self.manifest_name(&kind),
            kind,
            state,
        };
        self.pending_idle = None;
        self.cadence = Cadence::Identified;
        self.current = Some(state);
        if self.published.as_ref() == Some(&report) {
            return None;
        }
        self.published = Some(report.clone());
        Some(report)
    }

    /// Build a detector; `now` anchors the identity acquire window.
    pub(crate) const fn new(rules: Rc<RuleSet>, now: Instant) -> Self {
        Self {
            rules,
            identified: None,
            identified_occupant: None,
            last_pgid: None,
            vacant_streak: 0,
            next_identify: now,
            started: now,
            identified_at: None,
            published: None,
            pending_idle: None,
            current: None,
            published_occupant: None,
            pending_occupant: None,
            pending_hook: None,
            published_source: phux_resource::agent_state::EvidenceSource::Screen,
            live_session: None,
            live_session_held: false,
            stream_state: None,
            cadence: Cadence::Unidentified,
            #[cfg(test)]
            identity_source: IdentitySource::Kernel,
        }
    }

    /// Forget what we last published because the store changed underneath
    /// (`SET_METADATA`/`DELETE_METADATA` on `phux.agent/v1`). Re-arms exactly
    /// one republish, so an idle agent's record comes back after a delete.
    pub(crate) fn invalidate_published(&mut self) {
        self.published = None;
    }

    /// Wire the live-`AgentSession`-child probe (ADR-0103 §5).
    pub(crate) fn set_live_session_probe(&mut self, probe: live_session::LiveSessionProbe) {
        self.live_session = Some(probe);
    }

    /// Whether a live `AgentSession` child currently outranks the screen.
    /// Read every tick, never cached.
    pub(super) fn live_session_active(&self) -> bool {
        self.live_session.as_ref().is_some_and(|probe| probe())
    }

    /// Whether a screen derivation may publish while a live session exists.
    pub(super) const fn screen_may_publish(&self, derived: DetectedState, matched: bool) -> bool {
        let idle = matches!(derived, DetectedState::Idle);
        match self.stream_state {
            // Nothing asserted yet (or retracted): only `idle` may publish,
            // which also carries the identity for a session that has not
            // spoken.
            None => idle,
            // After `stop`, a positive idle may take over; the fail-safe
            // idle may not overwrite `done`.
            Some(DetectedState::Done) => idle && matched,
            // Still asserting `working`/`blocked`: the screen yields.
            Some(_) => false,
        }
    }

    /// The interval the actor should re-arm its detector timer at.
    pub(crate) const fn interval(&self) -> Duration {
        match self.cadence {
            Cadence::Unidentified => TICK_UNIDENTIFIED,
            Cadence::Identified => TICK_IDENTIFIED,
            Cadence::Confirming => TICK_CONFIRMING,
        }
    }

    /// Whether this tick needs a grid read.
    ///
    /// Unidentified panes never scan; confirming ticks always do. Otherwise a
    /// clean grid is skipped whatever the state: evaluation is a pure
    /// function of title and lines, and both dirty the actor's flag. A
    /// freshly identified pane (`current == None`) must scan regardless.
    pub(crate) fn wants_screen(&self, terminal_dirty: bool) -> bool {
        if self.identified.is_none() {
            return false;
        }
        if self.cadence == Cadence::Confirming {
            return true;
        }
        terminal_dirty || self.current.is_none()
    }
}
