//! Level-triggered agent-state detector (ADR-0046).
//!
//! # Why level-triggered
//!
//! The obvious design is edge-triggered: have the agent's shell hooks fire
//! on start and stop. It is also wrong. An edge-triggered reporter is lossy
//! — miss ONE transition (a crash, a `kill -9`, a hook that did not fire,
//! a race at startup) and the pane lies forever, with no path back to the
//! truth. A periodic re-derivation from the screen re-establishes ground
//! truth every tick and is therefore **self-healing**: however we got into
//! a wrong state, the next tick fixes it.
//!
//! # The trust model
//!
//! [`AgentDetector::tick`] implements it, in this order:
//!
//! 1. **Identify** the agent from the PTY's foreground process group.
//! 2. **Grace** — publish nothing for [`STARTUP_GRACE`] after IDENTIFICATION
//!    while a splash screen paints, so we never flash `blocked` at launch.
//!    Anchored there and not at pane creation: a pane seeds a shell, and the
//!    common way an agent appears is a human typing `claude` into it minutes
//!    later, which is exactly the launch the guard exists for.
//! 3. **Derive** a state from the manifest's region-scoped rules.
//!    *Fail safe:* identified but nothing matched means [`DetectedState::Idle`],
//!    **never** `Blocked`. A missed notification is cheap; a permanently-red
//!    sidebar destroys the user's trust in the whole feature, which is
//!    strictly worse than having no feature.
//! 4. **Hysteresis, asymmetric.** The UX thesis in one line: *transitions
//!    that demand attention are instant; transitions that release it are
//!    debounced.* `blocked` and `working` publish on the FIRST tick that
//!    sees them. `working -> idle` is the ambiguous one — the screen stopped
//!    saying "working" but does not positively say "idle" (a spinner cleared
//!    mid-redraw) — so it is held for [`IDLE_CONFIRMATIONS`] ticks, capped at
//!    [`IDLE_HOLD_CAP`], and the hold is BYPASSED when a rule supplies
//!    positive idle evidence.
//! 5. **Edge-filtered publish.** Only a genuinely changed tuple is emitted.
//!    An agent that is `working` and spewing output for ten minutes produces
//!    ZERO metadata writes and ZERO events.
//!
//! Staleness needs no TTL: identity is re-derived every [`IDENTIFY_RECHECK`],
//! so a dead agent's badge is actively *retracted* rather than left to spin
//! forever. A dead process must not lie.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private server module shared by the sibling terminal_actor / runtime / state modules"
)]

pub(crate) mod identify;
pub(crate) mod record;
pub(crate) mod regions;
pub(crate) mod rules;

use std::os::fd::RawFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use tracing::trace;

use rules::RuleSet;

/// Tick floor while no agent has been identified. Slow: there is nothing to
/// derive, and most panes are shells that will never host an agent.
pub(crate) const TICK_UNIDENTIFIED: Duration = Duration::from_millis(500);
/// Tick floor once an agent is identified.
pub(crate) const TICK_IDENTIFIED: Duration = Duration::from_millis(300);
/// Tick floor while confirming a `working -> idle` transition.
pub(crate) const TICK_CONFIRMING: Duration = Duration::from_millis(100);

/// How often identity is re-derived once it is known. Also the answer to
/// staleness: an exited agent is noticed within this window and retracted.
const IDENTIFY_RECHECK: Duration = Duration::from_secs(5);
/// A freshly-spawned pane is polled harder for this long, so an agent
/// launched at pane creation is identified promptly instead of waiting out
/// a full [`IDENTIFY_RECHECK`].
const IDENTIFY_ACQUIRE_WINDOW: Duration = Duration::from_millis(1500);
/// Identity poll interval inside [`IDENTIFY_ACQUIRE_WINDOW`].
const IDENTIFY_ACQUIRE_POLL: Duration = Duration::from_millis(500);
/// Publish nothing for this long after an agent is IDENTIFIED: agents paint a
/// splash screen, and a splash screen must not flash `blocked`.
///
/// Anchored at identification rather than at actor construction. A pane's seed
/// command is a shell; the agent is typed into it later, so an anchor at
/// construction expires before the agent that needs guarding even exists.
///
/// This is the production value; [`startup_grace`] is what `tick` consults,
/// so the integration tests can shrink the window without waiting out real
/// seconds of wall clock per positive case.
const STARTUP_GRACE: Duration = Duration::from_secs(3);

/// Test seam: override [`STARTUP_GRACE`] in milliseconds.
///
/// Follows the module's existing env-var idiom (`PHUX_AGENT_DETECT`,
/// `PHUX_AGENT_RULES_DIR` in `rules`). Integration tests set this to a few
/// hundred milliseconds so a positive detection does not sit out a real 3 s
/// splash-screen window per test; production never sets it, so the default
/// stays exactly [`STARTUP_GRACE`].
const ENV_STARTUP_GRACE_MS: &str = "PHUX_AGENT_STARTUP_GRACE_MS";

/// The effective startup grace: [`STARTUP_GRACE`] unless
/// [`ENV_STARTUP_GRACE_MS`] overrides it. Read once per process — the value
/// is consulted on every detector tick, and the env cannot change under a
/// running server.
fn startup_grace() -> Duration {
    static GRACE: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *GRACE.get_or_init(|| {
        std::env::var(ENV_STARTUP_GRACE_MS)
            .ok()
            .and_then(|ms| ms.parse::<u64>().ok())
            .map_or(STARTUP_GRACE, Duration::from_millis)
    })
}
/// Consecutive `idle` derivations required to release a `working` badge
/// absent positive idle evidence.
const IDLE_CONFIRMATIONS: u8 = 3;
/// Upper bound on the `working -> idle` hold, so a pathological screen
/// cannot pin a `working` badge indefinitely.
const IDLE_HOLD_CAP: Duration = Duration::from_millis(700);

/// A state the detector can derive. The `unknown` of the wire vocabulary is
/// not representable here: "we do not know" is expressed by publishing
/// nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetectedState {
    /// Available, not actively working.
    Idle,
    /// Actively doing work.
    Working,
    /// Waiting on a human.
    Blocked,
    /// Finished its task.
    Done,
}

impl DetectedState {
    /// The kebab-case wire word (`docs/spec/L3.md` §3.7).
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
        }
    }
}

/// What the detector concluded about a pane. The tuple that is edge-filtered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentReport {
    /// Open-vocabulary kind slug, e.g. `"claude"`.
    pub(crate) kind: String,
    /// Human-facing name for the record.
    pub(crate) name: String,
    /// The derived lifecycle state.
    pub(crate) state: DetectedState,
}

/// A detector output, drained by `runtime::client::spawn_agent_state_drain`.
///
/// Deliberately NOT a `phux_protocol` `AgentEvent`: that is a wire type, and
/// the detector introduces no wire surface. It rides the shipped
/// `SET_METADATA` / `METADATA_CHANGED` path for `phux.agent/v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentDetectEvent {
    /// Write this record.
    State(AgentReport),
    /// The agent is gone; delete the record (only if we authored it).
    Retract,
}

/// The result of one [`AgentDetector::tick`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DetectOutcome {
    /// Nothing to say. The overwhelmingly common outcome.
    Quiet,
    /// The derived tuple changed; publish it.
    Publish(AgentReport),
    /// The agent went away; retract the record.
    Retract,
}

/// The detector's current tick cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cadence {
    /// No agent identified yet.
    Unidentified,
    /// An agent is identified and its state is settled.
    Identified,
    /// A `working -> idle` transition is being confirmed.
    Confirming,
}

/// An in-flight `working -> idle` hold.
#[derive(Debug, Clone, Copy)]
struct PendingIdle {
    /// Consecutive `idle` derivations observed.
    confirmations: u8,
    /// When the hold began, for [`IDLE_HOLD_CAP`].
    since: Instant,
}

/// One pane's detector. Owned by the `TerminalActor`, driven by its own
/// interval — never by PTY bytes, so a chatty agent costs zero extra work.
pub(crate) struct AgentDetector {
    rules: Rc<RuleSet>,
    /// The agent kind currently running, if any.
    identified: Option<String>,
    /// When identity is next re-derived.
    next_identify: Instant,
    /// When this detector was constructed — anchors [`IDENTIFY_ACQUIRE_WINDOW`].
    started: Instant,
    /// When the current agent was IDENTIFIED — anchors [`STARTUP_GRACE`].
    ///
    /// Not `started`: a pane seeds a shell, and the overwhelmingly common way
    /// an agent appears is a human typing `claude` at that shell minutes
    /// later. Anchoring the grace at construction makes it a dead branch for
    /// exactly that case, which is the one the splash-screen guard exists for.
    identified_at: Option<Instant>,
    /// The last tuple we published. The edge filter.
    ///
    /// A model of the detector's own emissions, so it goes stale the moment
    /// something else writes the store. [`Self::invalidate_published`] is the
    /// store's way of saying so; see `crate::agent_state` for who calls it.
    published: Option<AgentReport>,
    /// An in-flight `working -> idle` hold.
    pending_idle: Option<PendingIdle>,
    /// The last state we derived (used when a scan is skipped).
    current: Option<DetectedState>,
    cadence: Cadence,
    /// Test seam: where [`Self::reidentify`] gets identity from.
    #[cfg(test)]
    identity_source: IdentitySource,
}

/// Where [`AgentDetector::reidentify`] reads identity from.
///
/// An agent typed at a shell prompt is the dominant flow and the one whose
/// acquisition sequencing was broken, and a unit test cannot conjure a real
/// foreground process group. The override replaces only the *kernel lookup*;
/// every line of the sequencing it feeds is the shipping one.
#[cfg(test)]
#[derive(Debug, Clone)]
enum IdentitySource {
    /// Ask the kernel, as production does.
    Kernel,
    /// Report this, with no PTY in sight.
    Forced(Option<String>),
}

impl std::fmt::Debug for AgentDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentDetector")
            .field("identified", &self.identified)
            .field("published", &self.published)
            .field("current", &self.current)
            .field("cadence", &self.cadence)
            .finish_non_exhaustive()
    }
}

impl AgentDetector {
    /// Build a detector. `now` anchors the identity acquire window, so the
    /// caller constructs this at the moment the child actually begins
    /// painting. The startup grace is anchored on IDENTIFICATION instead.
    pub(crate) const fn new(rules: Rc<RuleSet>, now: Instant) -> Self {
        Self {
            rules,
            identified: None,
            next_identify: now,
            started: now,
            identified_at: None,
            published: None,
            pending_idle: None,
            current: None,
            cadence: Cadence::Unidentified,
            #[cfg(test)]
            identity_source: IdentitySource::Kernel,
        }
    }

    /// Forget what we last published, because the metadata STORE changed
    /// underneath us (an explicit `SET_METADATA` or `DELETE_METADATA` on
    /// `phux.agent/v1`).
    ///
    /// [`Self::published`] is the detector's model of its own emissions, not
    /// of the store. Without this, a `DELETE` that hands the record back to
    /// the detector is a no-op for an idle agent: the next tick re-derives the
    /// same tuple, the edge filter suppresses it, and the pane simply has no
    /// agent record until the agent's state next changes — which, for an agent
    /// waiting on a human, is never. Clearing the filter re-arms exactly one
    /// republish; the "a working agent produces zero writes" invariant is
    /// untouched, because nothing calls this on the steady path.
    pub(crate) fn invalidate_published(&mut self) {
        self.published = None;
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
    /// The cheap steady state is mandatory: an idle pane whose grid has not
    /// changed since the last detector tick cannot have changed state, so we
    /// skip the scan entirely and the whole feature costs one timer wakeup.
    ///
    /// `terminal_dirty` is the actor's `agent_dirty_since_detect` flag — a
    /// flag scoped to THIS tick's cadence, not the 30 ms `tick_emit` one,
    /// which would read `false` almost always and skip every scan forever.
    ///
    /// While no agent is identified there is nothing to derive against, so no
    /// scan is needed at all. While confirming, we always scan: the whole
    /// point of the hold is to re-look.
    ///
    /// Otherwise a clean grid is skipped **whatever the current state**, not
    /// just when it is `idle`. `RuleSet::evaluate` is a pure function of
    /// `(title, lines)`, and both the PTY-chunk path and the resize path set
    /// the actor's dirty flag — a title change cannot sneak past a clean flag,
    /// because libghostty learns the new title from the same chunk. So a clean
    /// grid provably re-derives the same state, and re-projecting it is pure
    /// waste. That matters most for `blocked`: a permission prompt is a static
    /// screen that can sit there for hours, and it is exactly the state that
    /// used to force a full grid projection every 300 ms forever.
    ///
    /// `current.is_none()` is load-bearing, not decorative: a freshly
    /// identified pane has derived nothing yet and MUST scan, even if the grid
    /// has been clean since before the agent existed.
    pub(crate) fn wants_screen(&self, terminal_dirty: bool) -> bool {
        if self.identified.is_none() {
            return false;
        }
        if self.cadence == Cadence::Confirming {
            return true;
        }
        terminal_dirty || self.current.is_none()
    }

    /// One detector tick. See the module docs for the algorithm.
    ///
    /// `screen` is `None` when [`Self::wants_screen`] said the scan could be
    /// skipped; the detector then holds its last derivation rather than
    /// inventing a new one.
    pub(crate) fn tick(
        &mut self,
        now: Instant,
        master_fd: Option<RawFd>,
        title: &str,
        screen: Option<&[String]>,
    ) -> DetectOutcome {
        // 1. Identity.
        if now >= self.next_identify
            && let Some(outcome) = self.reidentify(now, master_fd)
        {
            return outcome;
        }
        let Some(kind) = self.identified.clone() else {
            return DetectOutcome::Quiet;
        };

        // 2. Startup grace. Identity may already be resolved; we simply do
        //    not publish while the splash paints. Anchored at IDENTIFICATION,
        //    so it covers the agent a human typed at a shell prompt — the
        //    common case — and not merely the pane's seed command.
        let anchor = self.identified_at.unwrap_or(self.started);
        if now < anchor + startup_grace() {
            return DetectOutcome::Quiet;
        }

        let Some(manifest) = self.rules.manifest(&kind) else {
            return DetectOutcome::Quiet;
        };
        let name = manifest.name.clone();

        // 3. Derive.
        let (derived, visible_idle) = match screen {
            // Scan skipped and nothing has ever been derived: we have no
            // evidence at all. HOLD, do not guess. Inventing `idle` here is
            // what latched a freshly-identified agent to `idle` forever —
            // `wants_screen` would then see `current == Some(Idle)` and never
            // ask for the scan that would have corrected it.
            None if self.current.is_none() => return DetectOutcome::Quiet,
            // Scan skipped: hold the last derivation, do not guess.
            None => (self.current.unwrap_or(DetectedState::Idle), false),
            Some(lines) => {
                let evaluation = manifest.evaluate(&regions::Screen { title, lines });
                if evaluation.freeze {
                    // A transcript viewer / model picker / pager. The screen
                    // carries NO information about agent state, so freeze the
                    // last derivation and publish nothing.
                    //
                    // Abandoning any in-flight `working -> idle` hold is
                    // mandatory, not incidental: the hold pins the 100 ms
                    // `Confirming` cadence AND an unconditional grid scan, and
                    // both exits from `Confirming` live in `settle_idle`,
                    // which this early return jumps over. A pager stays open
                    // for minutes. The hold simply restarts when it closes and
                    // a real derivation resumes.
                    //
                    // `current` is realigned with the badge we are freezing so
                    // that a later skipped scan holds THAT, rather than the
                    // half-derived `idle` the abandoned hold left behind —
                    // which would reopen the hold on the next screen-less tick,
                    // forever. `published` is untouched: freezing publishes
                    // nothing, by definition.
                    self.pending_idle = None;
                    self.cadence = Cadence::Identified;
                    self.current = self.published.as_ref().map(|r| r.state);
                    trace!(%kind, "agent-detect: screen carries no state; frozen");
                    return DetectOutcome::Quiet;
                }
                trace!(
                    %kind,
                    matched = evaluation.matched.as_deref().unwrap_or("<none>"),
                    visible_blocker = evaluation.visible_blocker,
                    visible_idle = evaluation.visible_idle,
                    visible_working = evaluation.visible_working,
                    "agent-detect: derived",
                );
                // FAIL SAFE: no state-bearing rule matched => Idle. Never
                // Blocked. This is the single most important line in the file.
                (
                    evaluation.state.unwrap_or(DetectedState::Idle),
                    evaluation.visible_idle,
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

    /// Re-derive identity from the kernel. Returns `Some(outcome)` when the
    /// tick is fully resolved by the identity step alone (the agent went away,
    /// or none has appeared yet).
    fn reidentify(&mut self, now: Instant, master_fd: Option<RawFd>) -> Option<DetectOutcome> {
        let found = self.resolve_identity(master_fd);
        self.apply_identity(now, found)
    }

    /// Ask the kernel which agent owns the PTY's foreground process group.
    #[cfg(not(test))]
    fn resolve_identity(&self, master_fd: Option<RawFd>) -> Option<String> {
        identify::foreground_agent(master_fd, &self.rules)
    }

    /// As above, honouring the [`IdentitySource`] test seam.
    #[cfg(test)]
    fn resolve_identity(&self, master_fd: Option<RawFd>) -> Option<String> {
        match &self.identity_source {
            IdentitySource::Kernel => identify::foreground_agent(master_fd, &self.rules),
            IdentitySource::Forced(kind) => kind.clone(),
        }
    }

    /// The pure half of [`Self::reidentify`]: everything that happens once the
    /// kernel has told us which agent (if any) owns the PTY's foreground
    /// process group. Split out so the acquisition sequencing — the part that
    /// latched a freshly identified agent to `idle` — is reachable from a test
    /// without a live PTY.
    fn apply_identity(&mut self, now: Instant, found: Option<String>) -> Option<DetectOutcome> {
        let acquiring = found.is_none() && now < self.started + IDENTIFY_ACQUIRE_WINDOW;
        self.next_identify = now
            + if acquiring {
                IDENTIFY_ACQUIRE_POLL
            } else {
                IDENTIFY_RECHECK
            };

        let Some(kind) = found else {
            self.identified = None;
            self.identified_at = None;
            self.pending_idle = None;
            self.current = None;
            self.cadence = Cadence::Unidentified;
            // A dead / exited agent must not keep a live badge. This is the
            // staleness answer: no TTL is needed, because identity is
            // re-derived on a fixed cadence and its absence is actionable.
            if self.published.take().is_some() {
                trace!("agent-detect: agent gone; retracting");
                return Some(DetectOutcome::Retract);
            }
            return Some(DetectOutcome::Quiet);
        };

        if self.identified.as_deref() != Some(kind.as_str()) {
            trace!(%kind, "agent-detect: identified");
            self.identified = Some(kind);
            // The splash screen paints from HERE, not from pane creation.
            self.identified_at = Some(now);
            // A different agent is a different pane, as far as we are
            // concerned. Nothing we previously derived applies.
            self.published = None;
            self.pending_idle = None;
            self.current = None;
            self.cadence = Cadence::Identified;
        }
        None
    }

    /// The `working -> idle` hold. Returns whether `Idle` may be published now.
    fn settle_idle(&mut self, now: Instant, visible_idle: bool) -> bool {
        let releasing_work = self
            .published
            .as_ref()
            .is_some_and(|r| r.state == DetectedState::Working);

        // Positive idle evidence, or we were not holding a `working` badge in
        // the first place: nothing to debounce.
        if visible_idle || !releasing_work {
            self.pending_idle = None;
            self.cadence = Cadence::Identified;
            return true;
        }

        // Ambiguous: the screen stopped saying "working" but does not say
        // "idle". Look again, fast, a few more times.
        self.cadence = Cadence::Confirming;
        let pending = self.pending_idle.get_or_insert(PendingIdle {
            confirmations: 0,
            since: now,
        });
        pending.confirmations = pending.confirmations.saturating_add(1);
        let settled = pending.confirmations >= IDLE_CONFIRMATIONS
            || now.duration_since(pending.since) >= IDLE_HOLD_CAP;
        if settled {
            self.pending_idle = None;
            self.cadence = Cadence::Identified;
        }
        settled
    }

    /// Test seam: force an identity without a live PTY, so the hysteresis
    /// state machine — which is pure — can be driven by a fake clock. Also used
    /// by the `terminal_actor` tests that pin `detect_tick`'s contract with the
    /// dirty flag, which only bites once an agent is identified.
    #[cfg(test)]
    pub(crate) fn force_identity(&mut self, kind: &str, now: Instant) {
        self.identified = Some(kind.to_owned());
        self.identified_at = Some(now);
        self.cadence = Cadence::Identified;
        // Push the next identity poll far out; `tick` must not try to read a
        // (nonexistent) PTY during the state-machine tests.
        self.next_identify = now + Duration::from_secs(3600);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use super::rules::{ManifestSpec, RuleSet};
    use super::{
        AgentDetector, DetectOutcome, DetectedState, IDLE_CONFIRMATIONS, STARTUP_GRACE,
        TICK_CONFIRMING, TICK_IDENTIFIED,
    };

    /// A manifest exercising every arm of the hysteresis machine with
    /// unambiguous synthetic screens.
    const MANIFEST: &str = r#"
kind = "t"
name = "t-agent"
binaries = ["t"]

[[rules]]
id = "working"
state = "working"
priority = 50
region = "viewport"
visible-working = true
match = { contains = "WORKING" }

[[rules]]
id = "blocked"
state = "blocked"
priority = 80
region = "viewport"
visible-blocker = true
match = { contains = "BLOCKED" }

[[rules]]
id = "done"
state = "done"
priority = 70
region = "viewport"
match = { contains = "DONE" }

[[rules]]
id = "idle-positive"
state = "idle"
priority = 40
region = "viewport"
visible-idle = true
match = { contains = "IDLE" }

[[rules]]
id = "pager"
priority = 200
region = "viewport"
skip-state-update = true
match = { contains = "PAGER" }
"#;

    struct Harness {
        detector: AgentDetector,
        now: Instant,
        /// The actor's `agent_dirty_since_detect` flag, for the tests that
        /// replay the actor's real tick sequence.
        dirty: bool,
    }

    impl Harness {
        fn new() -> Self {
            let mut h = Self::unidentified();
            let now = h.now;
            h.detector.force_identity("t", now);
            // Step past the startup grace unless a test opts out.
            h
        }

        /// A detector on a pane that is still just a shell — no agent has been
        /// identified, and identity is whatever the override says.
        fn unidentified() -> Self {
            let spec: ManifestSpec = toml::from_str(MANIFEST).expect("manifest parses");
            let mut set = RuleSet::default();
            set.install(spec).expect("compiles");
            let now = Instant::now();
            Self {
                detector: AgentDetector::new(Rc::new(set), now),
                now,
                dirty: false,
            }
        }

        fn past_grace(mut self) -> Self {
            self.now += STARTUP_GRACE + Duration::from_millis(1);
            self
        }

        /// Replay the `TerminalActor`'s REAL `detect_tick` sequence: advance
        /// the clock by the detector's own interval, ask `wants_screen` (which
        /// is asked BEFORE identity resolves, and that is the whole point),
        /// consume the dirty flag only if a scan actually happens, then tick.
        ///
        /// Distinct from [`Self::tick`], which hands the detector a screen
        /// unconditionally: the bug this replays lives entirely in the
        /// ordering, so a test that skips the ordering cannot see it.
        fn actor_tick(&mut self, screen: &str) -> DetectOutcome {
            self.now += self.detector.interval();
            let lines = [screen.to_owned()];
            let scan = self.detector.wants_screen(self.dirty);
            if scan {
                self.dirty = false;
            }
            self.detector
                .tick(self.now, None, "", scan.then_some(&lines[..]))
        }

        /// The human types an agent's name at the pane's shell prompt. The
        /// agent paints, which sets the actor's dirty flag.
        fn launch_agent(&mut self, kind: &str) {
            self.detector.identity_source = super::IdentitySource::Forced(Some(kind.to_owned()));
            self.dirty = true;
        }

        /// Advance the fake clock by the detector's own current interval and
        /// feed it a screen. This is what the actor's timer arm does.
        fn tick(&mut self, screen: &str) -> DetectOutcome {
            self.now += self.detector.interval();
            let lines = vec![screen.to_owned()];
            self.detector.tick(self.now, None, "", Some(&lines))
        }

        /// Tick with the scan skipped (the cheap steady state).
        fn tick_no_scan(&mut self) -> DetectOutcome {
            self.now += self.detector.interval();
            self.detector.tick(self.now, None, "", None)
        }

        fn state(&self) -> Option<DetectedState> {
            self.detector.published.as_ref().map(|r| r.state)
        }
    }

    fn published(outcome: &DetectOutcome) -> DetectedState {
        match outcome {
            DetectOutcome::Publish(report) => report.state,
            other => panic!("expected a publish, got {other:?}"),
        }
    }

    #[test]
    fn blocked_publishes_on_the_first_tick() {
        let mut h = Harness::new().past_grace();
        let out = h.tick("BLOCKED");
        assert_eq!(published(&out), DetectedState::Blocked);
    }

    #[test]
    fn working_publishes_on_the_first_tick() {
        let mut h = Harness::new().past_grace();
        let out = h.tick("WORKING");
        assert_eq!(published(&out), DetectedState::Working);
    }

    #[test]
    fn done_publishes_on_the_first_tick() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("DONE")), DetectedState::Done);
    }

    #[test]
    fn report_carries_the_manifest_kind_and_name() {
        let mut h = Harness::new().past_grace();
        match h.tick("WORKING") {
            DetectOutcome::Publish(r) => {
                assert_eq!(r.kind, "t");
                assert_eq!(r.name, "t-agent");
            }
            other => panic!("expected a publish, got {other:?}"),
        }
    }

    /// The ambiguous transition: the screen stopped saying "working" but does
    /// not positively say "idle". Hold, then release.
    #[test]
    fn working_to_ambiguous_idle_is_held_for_three_confirmations() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);

        for i in 1..IDLE_CONFIRMATIONS {
            assert_eq!(
                h.tick("nothing matches here"),
                DetectOutcome::Quiet,
                "confirmation {i} must not publish yet",
            );
            assert_eq!(h.state(), Some(DetectedState::Working), "badge still held");
        }
        let out = h.tick("nothing matches here");
        assert_eq!(
            published(&out),
            DetectedState::Idle,
            "released on the third"
        );
    }

    /// The hold ticks FAST, so the debounce costs ~300 ms of latency, not
    /// three lazy 300 ms ticks.
    #[test]
    fn the_hold_runs_at_the_confirming_cadence() {
        let mut h = Harness::new().past_grace();
        h.tick("WORKING");
        assert_eq!(h.detector.interval(), TICK_IDENTIFIED);
        h.tick("nothing");
        assert_eq!(
            h.detector.interval(),
            TICK_CONFIRMING,
            "the detector must look again quickly while confirming",
        );
    }

    /// Positive idle evidence bypasses the hold entirely: the screen is not
    /// merely "no longer saying working", it is affirmatively saying idle.
    #[test]
    fn visible_idle_bypasses_the_hold() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
        let out = h.tick("IDLE");
        assert_eq!(published(&out), DetectedState::Idle, "no debounce");
        assert_eq!(h.detector.interval(), TICK_IDENTIFIED);
    }

    /// The hold has a wall-clock cap, so a screen that never resolves cannot
    /// pin a `working` badge forever.
    #[test]
    fn the_hold_is_capped_in_wall_clock_time() {
        let mut h = Harness::new().past_grace();
        h.tick("WORKING");
        // One ambiguous tick to open the hold ...
        assert_eq!(h.tick("nothing"), DetectOutcome::Quiet);
        // ... then jump the clock past the cap: the very next tick releases,
        // without waiting for the confirmation count.
        h.now += super::IDLE_HOLD_CAP;
        let lines = vec!["nothing".to_owned()];
        let out = h.detector.tick(h.now, None, "", Some(&lines));
        assert_eq!(published(&out), DetectedState::Idle);
    }

    /// Blocked interrupts a pending idle hold immediately — attention wins.
    #[test]
    fn blocked_during_the_idle_hold_publishes_at_once() {
        let mut h = Harness::new().past_grace();
        h.tick("WORKING");
        assert_eq!(h.tick("nothing"), DetectOutcome::Quiet);
        let out = h.tick("BLOCKED");
        assert_eq!(published(&out), DetectedState::Blocked);
        assert!(h.detector.pending_idle.is_none(), "hold cleared");
    }

    /// Going idle from a non-working state is not ambiguous at all, so it is
    /// not debounced.
    #[test]
    fn idle_from_blocked_is_not_debounced() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("BLOCKED")), DetectedState::Blocked);
        let out = h.tick("nothing matches");
        assert_eq!(published(&out), DetectedState::Idle);
    }

    /// A pager / transcript viewer / model picker carries no information.
    /// Freeze; do not guess.
    #[test]
    fn skip_state_update_freezes_the_previous_state() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
        assert_eq!(h.tick("PAGER"), DetectOutcome::Quiet);
        assert_eq!(h.state(), Some(DetectedState::Working), "badge frozen");
        // And it does not even start an idle hold.
        assert!(h.detector.pending_idle.is_none());
        // Leaving the pager resumes normal derivation.
        assert_eq!(published(&h.tick("IDLE")), DetectedState::Idle);
    }

    /// A freeze rule that ALSO matches a blocked screen still freezes — but
    /// our shipped manifest deliberately excludes that overlap. This pins the
    /// documented precedence.
    #[test]
    fn freeze_outranks_every_state_bearing_rule() {
        let mut h = Harness::new().past_grace();
        assert_eq!(h.tick("PAGER and BLOCKED"), DetectOutcome::Quiet);
        assert_eq!(h.state(), None);
    }

    /// THE fail-safe. An identified agent whose screen matches nothing is
    /// idle, never blocked.
    #[test]
    fn an_identified_agent_with_no_matching_rule_is_idle_never_blocked() {
        let mut h = Harness::new().past_grace();
        let out = h.tick("total gibberish that matches nothing");
        assert_eq!(published(&out), DetectedState::Idle);
        assert_ne!(h.state(), Some(DetectedState::Blocked));
    }

    /// THE efficiency contract. A `working` agent spewing output for ten
    /// minutes must produce exactly ONE metadata write.
    #[test]
    fn a_long_working_run_publishes_exactly_once() {
        let mut h = Harness::new().past_grace();
        let mut publishes = 0;
        for _ in 0..10 {
            if matches!(h.tick("WORKING"), DetectOutcome::Publish(_)) {
                publishes += 1;
            }
        }
        assert_eq!(
            publishes, 1,
            "edge-filtered: only the transition is an event"
        );
    }

    #[test]
    fn a_long_idle_run_publishes_exactly_once() {
        let mut h = Harness::new().past_grace();
        let mut publishes = 0;
        for _ in 0..10 {
            if matches!(h.tick("IDLE"), DetectOutcome::Publish(_)) {
                publishes += 1;
            }
        }
        assert_eq!(publishes, 1);
    }

    /// The startup grace: an agent painting a splash screen that happens to
    /// contain a scary word must not flash `blocked` at launch.
    #[test]
    fn the_startup_grace_suppresses_publication() {
        let mut h = Harness::new();
        // Not past grace. Tick across the whole window; `elapsed` tracks where
        // the NEXT tick will land, so the loop never steps onto the boundary.
        let mut elapsed = TICK_IDENTIFIED;
        while elapsed < STARTUP_GRACE {
            assert_eq!(h.tick("BLOCKED"), DetectOutcome::Quiet, "silent in grace");
            elapsed += TICK_IDENTIFIED;
        }
        assert_eq!(h.state(), None);
        // Once the grace expires, the very same screen publishes.
        h.now += STARTUP_GRACE;
        let lines = vec!["BLOCKED".to_owned()];
        let out = h.detector.tick(h.now, None, "", Some(&lines));
        assert_eq!(published(&out), DetectedState::Blocked);
    }

    /// The cheap steady state: with the scan skipped, the detector holds its
    /// last derivation and says nothing.
    #[test]
    fn a_skipped_scan_holds_the_last_state_and_is_quiet() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
        assert_eq!(h.tick_no_scan(), DetectOutcome::Quiet);
        assert_eq!(h.state(), Some(DetectedState::Working));
    }

    #[test]
    fn wants_screen_skips_the_scan_whenever_the_grid_is_clean() {
        let mut h = Harness::new().past_grace();
        h.tick("IDLE");
        assert!(!h.detector.wants_screen(false), "idle + clean => skip");
        assert!(h.detector.wants_screen(true), "dirty => scan");
    }

    /// A `blocked` pane is a permission prompt: a STATIC screen that waits on a
    /// human for minutes or hours, emitting not one byte. Re-projecting the
    /// whole libghostty grid every 300 ms to re-derive a result that provably
    /// cannot have changed abandons the cheap steady state in precisely the
    /// state that is by definition the longest-lived.
    #[test]
    fn a_blocked_pane_with_a_clean_grid_does_not_rescan() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("BLOCKED")), DetectedState::Blocked);
        assert!(
            !h.detector.wants_screen(false),
            "a clean grid cannot yield a different derivation, blocked or not",
        );
        // ... and the held state is still correct, and still silent.
        assert_eq!(h.tick_no_scan(), DetectOutcome::Quiet);
        assert_eq!(h.state(), Some(DetectedState::Blocked), "badge held");
        // The instant bytes arrive, it looks again.
        assert!(h.detector.wants_screen(true));
    }

    /// The `current.is_none()` disjunct in `wants_screen` is load-bearing: a
    /// freshly identified pane has derived nothing yet and MUST scan, even
    /// though the grid has been clean since before the agent existed.
    #[test]
    fn a_freshly_identified_pane_scans_even_with_a_clean_grid() {
        let h = Harness::new().past_grace();
        assert!(
            h.detector.wants_screen(false),
            "nothing derived yet: the detector has no state to hold",
        );
    }

    #[test]
    fn wants_screen_is_false_while_unidentified() {
        let spec: ManifestSpec = toml::from_str(MANIFEST).expect("parses");
        let mut set = RuleSet::default();
        set.install(spec).expect("compiles");
        let detector = AgentDetector::new(Rc::new(set), Instant::now());
        assert!(!detector.wants_screen(true), "nothing to derive against");
        assert_eq!(detector.interval(), super::TICK_UNIDENTIFIED);
    }

    /// An unidentified pane (no PTY / no agent) never publishes anything, and
    /// never retracts anything it did not write.
    #[test]
    fn an_unidentified_pane_is_silent() {
        let spec: ManifestSpec = toml::from_str(MANIFEST).expect("parses");
        let mut set = RuleSet::default();
        set.install(spec).expect("compiles");
        let now = Instant::now();
        let mut detector = AgentDetector::new(Rc::new(set), now);
        for i in 0..5 {
            let at = now + Duration::from_secs(i * 6);
            assert_eq!(detector.tick(at, None, "", None), DetectOutcome::Quiet);
        }
    }

    /// A dead agent must not lie: when identity disappears, the record we
    /// wrote is retracted, not left to spin forever.
    #[test]
    fn losing_identity_retracts_a_published_record() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);

        // Let the identity poll come due. `master_fd = None` makes
        // `foreground_agent` return `None` — the agent is gone.
        h.now += super::IDENTIFY_RECHECK;
        h.detector.next_identify = h.now;
        let lines = vec!["WORKING".to_owned()];
        assert_eq!(
            h.detector.tick(h.now, None, "", Some(&lines)),
            DetectOutcome::Retract,
        );
        assert_eq!(h.state(), None, "the badge is gone");

        // And it retracts exactly once.
        h.detector.next_identify = h.now;
        assert_eq!(
            h.detector.tick(h.now, None, "", Some(&lines)),
            DetectOutcome::Quiet,
        );
    }

    // --- the mid-pane agent launch (the dominant interactive flow) ---------

    /// THE latch. A pane runs a shell; two minutes later the human types
    /// `claude`, which paints a permission dialog and then goes silent behind
    /// it, waiting on them.
    ///
    /// `wants_screen` is asked BEFORE identity resolves, and it is `false` for
    /// the whole of a pane's unidentified life — so the tick that first
    /// identifies the agent reads no screen. Consuming the dirty flag on that
    /// tick threw away the only evidence that the dialog had ever been
    /// painted; deriving `idle` from the screen it never read then latched:
    /// `wants_screen` saw `current == Some(Idle)` and never asked for the scan
    /// that would have corrected it, and the agent — being blocked — produced
    /// no further bytes to re-dirty the grid. The pane was BLOCKED and the
    /// sidebar said `idle`, permanently, in exactly the state the whole
    /// feature exists to surface.
    #[test]
    fn a_mid_pane_agent_launch_does_not_latch_to_idle() {
        let mut h = Harness::unidentified();

        // Two minutes of plain shell. The acquire window lapsed long ago.
        for _ in 0..240 {
            assert_eq!(h.actor_tick("$ "), DetectOutcome::Quiet);
        }

        // The human types the agent's name. It paints its dialog, dirtying the
        // grid, and then waits for an answer — emitting nothing further, ever.
        h.launch_agent("t");
        assert!(
            !h.detector.wants_screen(h.dirty),
            "unidentified: there is nothing to derive against, so no scan",
        );

        // The identification tick. It reads no screen, so it must publish
        // NOTHING — and it must not eat the dirty bit.
        assert_eq!(
            h.actor_tick("BLOCKED"),
            DetectOutcome::Quiet,
            "no screen was read: hold, do not invent `idle` from zero evidence",
        );
        assert!(
            h.dirty,
            "a tick that performed no scan must not consume the evidence that a scan is owed",
        );

        // Now run for three minutes of a static, silent, blocked screen.
        let mut publishes = Vec::new();
        for _ in 0..600 {
            if let DetectOutcome::Publish(report) = h.actor_tick("BLOCKED") {
                publishes.push(report.state);
            }
        }
        assert_eq!(
            publishes,
            vec![DetectedState::Blocked],
            "the truth, published exactly once — never a fabricated `idle`",
        );
        assert_eq!(h.state(), Some(DetectedState::Blocked));
    }

    /// The other half of the latch, isolated. A screen-less tick that has
    /// NOTHING derived yet holds zero evidence, so it must hold — not guess.
    /// `current.unwrap_or(Idle)` is sound only when `current` is `Some` (the
    /// "hold, do not guess" contract); with `current == None` it invents a
    /// state from a screen it never read, and the guess is self-reinforcing
    /// through `wants_screen`. Reachable whenever `viewport_lines` fails to
    /// project the grid, and the backstop if the grace anchor ever regresses.
    #[test]
    fn a_screenless_tick_with_nothing_derived_publishes_nothing() {
        let mut h = Harness::new().past_grace();
        assert_eq!(h.detector.current, None, "nothing derived yet");
        assert_eq!(
            h.tick_no_scan(),
            DetectOutcome::Quiet,
            "zero evidence: hold, do not invent `idle`",
        );
        assert_eq!(h.state(), None, "nothing was published");
        assert!(
            h.detector.wants_screen(false),
            "and it still owes itself a scan — the guess must not latch",
        );
    }

    /// The startup grace is anchored at IDENTIFICATION, not at construction.
    /// A pane seeds a shell and the agent is typed into it minutes later, so an
    /// anchor at pane creation is a dead branch for precisely the launch the
    /// splash-screen guard exists to cover.
    #[test]
    fn the_startup_grace_is_anchored_at_identification_not_pane_creation() {
        let mut h = Harness::unidentified();
        // Ten minutes of shell: any grace anchored at construction is long
        // gone.
        h.now += Duration::from_secs(600);
        h.launch_agent("t");

        // The identification tick, then the agent's splash screen — which here
        // contains a word a `blocked` rule matches.
        assert_eq!(h.actor_tick("BLOCKED"), DetectOutcome::Quiet, "identifying");
        for i in 1..10 {
            assert_eq!(
                h.actor_tick("BLOCKED"),
                DetectOutcome::Quiet,
                "tick {i} lands inside the grace: the splash must not flash `blocked`",
            );
        }
        assert_eq!(h.state(), None, "nothing was published while it painted");

        // And the grace does expire.
        assert_eq!(published(&h.actor_tick("BLOCKED")), DetectedState::Blocked);
    }

    // --- the edge filter is a model of OUR emissions, not of the store ------

    /// A `DELETE_METADATA` hands the record back to the detector (ADR-0046 §E).
    /// But the edge filter still holds the tuple the detector last derived, so
    /// the next tick derives the same thing, suppresses it, and writes nothing
    /// — the record simply does not come back until the agent's state next
    /// changes, which for an idle agent waiting on a human is NEVER. The store
    /// therefore has to be able to say "forget what you published".
    #[test]
    fn invalidating_the_edge_filter_republishes_an_unchanged_state() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("IDLE")), DetectedState::Idle);
        assert_eq!(
            h.tick("IDLE"),
            DetectOutcome::Quiet,
            "steady state is quiet"
        );

        // `phux agent clear`: the row is gone from the store.
        h.detector.invalidate_published();

        assert_eq!(
            published(&h.tick("IDLE")),
            DetectedState::Idle,
            "the detector resumes: the record comes back on the next tick",
        );
        assert_eq!(
            h.tick("IDLE"),
            DetectOutcome::Quiet,
            "and it is a re-arm, not a repeat — the filter closes again at once",
        );
    }

    /// The same, on the path that actually runs: an idle agent's grid is clean,
    /// so no scan happens at all. The republish must not depend on one.
    #[test]
    fn an_invalidated_idle_agent_republishes_without_a_scan() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("IDLE")), DetectedState::Idle);
        assert!(
            !h.detector.wants_screen(false),
            "idle + clean grid: the scan is skipped, as designed",
        );
        h.detector.invalidate_published();
        assert_eq!(published(&h.tick_no_scan()), DetectedState::Idle);
    }

    // --- freeze ------------------------------------------------------------

    /// A pager opened DURING a `working -> idle` hold must not pin the 100 ms
    /// confirming cadence — and the unconditional grid scan that rides it — for
    /// as long as it stays open, which is minutes. Both exits from `Confirming`
    /// live in `settle_idle`, which the freeze branch returns before reaching.
    #[test]
    fn a_freeze_during_the_idle_hold_drops_the_hold_and_the_fast_cadence() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
        // The turn ends ambiguously: the hold opens, the cadence goes fast.
        assert_eq!(h.tick("nothing matches"), DetectOutcome::Quiet);
        assert_eq!(h.detector.interval(), TICK_CONFIRMING);
        assert!(h.detector.pending_idle.is_some(), "holding");

        // The user hits ctrl+o and reads the transcript for three minutes.
        for _ in 0..600 {
            assert_eq!(h.tick("PAGER"), DetectOutcome::Quiet);
        }
        assert!(
            h.detector.pending_idle.is_none(),
            "the in-flight hold is abandoned, not pinned",
        );
        assert_eq!(
            h.detector.interval(),
            TICK_IDENTIFIED,
            "and the cadence falls back to the settled one",
        );
        assert!(
            !h.detector.wants_screen(false),
            "a frozen screen with a clean grid is not re-projected 10x a second",
        );
        assert_eq!(
            h.state(),
            Some(DetectedState::Working),
            "the badge is still frozen exactly where it was",
        );

        // Closing the pager resumes normal derivation, and the hold restarts
        // from scratch rather than resuming a stale confirmation count.
        for i in 1..IDLE_CONFIRMATIONS {
            assert_eq!(
                h.tick("nothing matches"),
                DetectOutcome::Quiet,
                "confirmation {i} of the restarted hold",
            );
        }
        assert_eq!(published(&h.tick("nothing matches")), DetectedState::Idle);
    }

    /// Title rules outrank screen rules — the end-to-end version of the unit
    /// test in `rules`, driven through `tick`.
    #[test]
    fn the_title_outranks_the_screen() {
        let spec: ManifestSpec = toml::from_str(
            r#"
kind = "t"
binaries = ["t"]
[[rules]]
id = "title-working"
state = "working"
priority = 1
region = "title"
match = { contains = "busy" }
[[rules]]
id = "screen-idle"
state = "idle"
priority = 99
region = "viewport"
visible-idle = true
match = { contains = "IDLE" }
"#,
        )
        .expect("parses");
        let mut set = RuleSet::default();
        set.install(spec).expect("compiles");
        let now = Instant::now();
        let mut detector = AgentDetector::new(Rc::new(set), now);
        detector.force_identity("t", now);
        let at = now + STARTUP_GRACE + Duration::from_millis(1);
        let lines = vec!["IDLE".to_owned()];
        let out = detector.tick(at, None, "busy", Some(&lines));
        assert_eq!(published(&out), DetectedState::Working);
    }
}
