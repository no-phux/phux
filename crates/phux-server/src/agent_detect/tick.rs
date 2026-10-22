//! One detector tick: identify the occupant, then derive and publish its state.

use std::os::fd::RawFd;
use std::time::Instant;

use tracing::trace;

use super::{
    AgentDetector, AgentReport, Cadence, DetectOutcome, DetectedState, HOOK_IDENTITY_GRACE,
    regions, startup_grace,
};

impl AgentDetector {
    /// One detector tick (test entry; see [`Self::tick_for_pane`]). `screen`
    /// is `None` when [`Self::wants_screen`] skipped the scan.
    #[cfg(test)]
    pub(crate) fn tick(
        &mut self,
        now: Instant,
        master_fd: Option<RawFd>,
        title: &str,
        progress: &str,
        screen: Option<&[String]>,
    ) -> DetectOutcome {
        self.tick_for_pane(now, master_fd, None, title, progress, screen)
    }

    /// Production tick with the pane's original child pid, used to distinguish
    /// that shell from another foreground job.
    pub(crate) fn tick_for_pane(
        &mut self,
        now: Instant,
        master_fd: Option<RawFd>,
        pane_child_pid: Option<i32>,
        title: &str,
        progress: &str,
        screen: Option<&[String]>,
    ) -> DetectOutcome {
        // 0. Rank: a live `AgentSession` child outranks the screen
        //    (ADR-0103 §5). Read once so every step agrees.
        let live_session = self.live_session_active();
        if self.live_session_held && !live_session {
            // The session ended: the store holds the stream's last word, so
            // re-arm one republish from the screen.
            self.invalidate_published();
            self.stream_state = None;
        }
        self.live_session_held = live_session;

        // 1. Identity.
        if let Some(outcome) = self.maybe_identify(now, master_fd, pane_child_pid) {
            return outcome;
        }
        let Some(kind) = self.identified.clone() else {
            return DetectOutcome::Quiet;
        };

        // A hook that beat identity publishes once the process resolves,
        // but only within `HOOK_IDENTITY_GRACE`.
        if let Some((state, reported_at)) = self.pending_hook.take()
            && now.saturating_duration_since(reported_at) <= HOOK_IDENTITY_GRACE
        {
            let report = AgentReport {
                name: self.manifest_name(&kind),
                kind,
                state,
            };
            self.pending_idle = None;
            self.cadence = Cadence::Identified;
            self.current = Some(state);
            self.published = Some(report.clone());
            return DetectOutcome::Publish(report);
        }

        // 2. Startup grace, anchored at identification.
        let anchor = self.identified_at.unwrap_or(self.started);
        if now < anchor + startup_grace() {
            return DetectOutcome::Quiet;
        }

        let Some(manifest) = self.rules.manifest(&kind) else {
            return DetectOutcome::Quiet;
        };
        let name = manifest.name.clone();

        // 3. Derive. `matched` separates "a rule said idle" from the
        //    fail-safe idle, which step 4b treats differently.
        let (derived, visible_idle, matched) = match screen {
            // Nothing ever derived and no scan: hold. Guessing `idle` here
            // would latch, since `wants_screen` would then never scan.
            None if self.current.is_none() => return DetectOutcome::Quiet,
            // Scan skipped: hold the last derivation (not a fresh match).
            None => (self.current.unwrap_or(DetectedState::Idle), false, false),
            Some(lines) => {
                let evaluation = manifest.evaluate(&regions::Screen {
                    title,
                    progress,
                    lines,
                });
                if evaluation.freeze {
                    // A pager or picker carries no state: freeze and publish
                    // nothing. Drop any idle hold (its exits live in
                    // `settle_idle`, which this skips) and realign `current`
                    // with the frozen badge so a later skipped scan holds it.
                    self.pending_idle = None;
                    self.cadence = Cadence::Identified;
                    self.current = self.published.as_ref().map(|r| r.state);
                    trace!(%kind, "agent-detect: screen carries no state; frozen");
                    return DetectOutcome::Quiet;
                }
                trace!(
                    %kind,
                    matched = evaluation.matched.as_deref().unwrap_or("<none>"),
                    visible_idle = evaluation.visible_idle,
                    "agent-detect: derived",
                );
                // FAIL SAFE: no state-bearing rule matched => Idle, never Blocked.
                (
                    evaluation.state.unwrap_or(DetectedState::Idle),
                    evaluation.visible_idle,
                    evaluation.state.is_some(),
                )
            }
        };

        // 4. Asymmetric hysteresis.
        let publishable = match derived {
            DetectedState::Blocked | DetectedState::Working | DetectedState::Done => {
                self.pending_idle = None;
                self.cadence = Cadence::Identified;
                true
            }
            DetectedState::Idle => self.settle_idle(now, visible_idle),
        };
        self.current = Some(derived);
        if !publishable {
            return DetectOutcome::Quiet;
        }

        // 4b. Precedence (ADR-0103 §5): with a live child, the screen may
        //     not publish `working`/`blocked`/`done`, and its `idle` only per
        //     `screen_may_publish`. `published` is not advanced by a
        //     suppressed write.
        if live_session && !self.screen_may_publish(derived, matched) {
            trace!(
                %kind,
                ?derived,
                matched,
                stream = ?self.stream_state,
                "agent-detect: a live agent session outranks the screen; not publishing",
            );
            return DetectOutcome::Quiet;
        }

        // 5. Edge filter.
        let report = AgentReport {
            kind,
            name,
            state: derived,
        };
        if self.published.as_ref() == Some(&report) {
            return DetectOutcome::Quiet;
        }
        self.published = Some(report.clone());
        DetectOutcome::Publish(report)
    }
}
