//! Level-triggered agent-state detector (ADR-0046).
//!
//! Re-deriving state from the screen every tick is self-healing, unlike
//! edge-triggered hooks, where one missed transition lies forever.
//! [`AgentDetector::tick`] does, in order:
//!
//! 1. **Identify** the agent from the PTY's foreground process group.
//! 2. **Grace**: publish nothing for [`STARTUP_GRACE`] after identification,
//!    so a splash screen never flashes `blocked`.
//! 3. **Derive** from the manifest's rules. Nothing matched means `Idle`,
//!    never `Blocked`.
//! 4. **Asymmetric hysteresis**: `blocked`/`working` publish at once;
//!    `working -> idle` is held for [`IDLE_CONFIRMATIONS`] ticks (capped at
//!    [`IDLE_HOLD_CAP`]) unless a rule gives positive idle evidence.
//! 5. **Edge filter**: only a changed tuple is emitted.
//!
//! Identity distinguishes agent, observed non-agent, and unanswerable
//! ([`identify::Occupancy`]). Only a confirmed vacancy, seen
//! [`VACANT_CONFIRMATIONS`] times, retracts; a failed query changes nothing.
//! That is what makes a retraction trustworthy enough to withdraw a human's
//! declaration (`docs/spec/L3.md` §3.7). Occupants are `(pgid, start time)`
//! pairs, so a restart or a recycled pgid is a new occupant.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private server module shared by the sibling terminal_actor / runtime / state modules"
)]

pub(crate) mod identify;
pub(crate) mod live_session;
pub(crate) mod record;

// Manifest evaluation lives in `phux-agent-rules`.
pub(crate) use phux_agent_rules::DetectedState;
pub(crate) use phux_agent_rules::regions;
pub(crate) use phux_agent_rules::rules;

use std::os::fd::RawFd;
use std::rc::Rc;
use std::time::{Duration, Instant};

use tracing::trace;

use rules::RuleSet;

/// Tick floor while no agent has been identified.
pub(crate) const TICK_UNIDENTIFIED: Duration = Duration::from_millis(500);
/// Tick floor once an agent is identified.
pub(crate) const TICK_IDENTIFIED: Duration = Duration::from_millis(300);
/// Tick floor while confirming a `working -> idle` transition.
pub(crate) const TICK_CONFIRMING: Duration = Duration::from_millis(100);

/// How often identity is re-derived once known; also bounds how long a dead
/// agent's badge survives.
const IDENTIFY_RECHECK: Duration = Duration::from_secs(5);

/// Test seam: override [`IDENTIFY_RECHECK`] in milliseconds.
const ENV_IDENTIFY_RECHECK_MS: &str = "PHUX_AGENT_IDENTIFY_RECHECK_MS";

/// The effective identity recheck interval, read once per process.
fn identify_recheck() -> Duration {
    static RECHECK: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *RECHECK.get_or_init(|| {
        std::env::var(ENV_IDENTIFY_RECHECK_MS)
            .ok()
            .and_then(|ms| ms.parse::<u64>().ok())
            .map_or(IDENTIFY_RECHECK, Duration::from_millis)
    })
}

/// Consecutive confirmed-vacant observations before an agent is declared
/// gone, so briefly handing the terminal to a pager or build does not
/// withdraw its badge.
const VACANT_CONFIRMATIONS: u8 = 2;
/// A freshly spawned pane is polled harder for this long.
const IDENTIFY_ACQUIRE_WINDOW: Duration = Duration::from_millis(1500);
/// How long a hook that beat the first identity poll stays applicable.
const HOOK_IDENTITY_GRACE: Duration = Duration::from_millis(1500);
/// Identity poll interval inside [`IDENTIFY_ACQUIRE_WINDOW`].
const IDENTIFY_ACQUIRE_POLL: Duration = Duration::from_millis(500);
/// Publish nothing for this long after an agent is identified (splash
/// screens). Anchored at identification because agents are usually typed
/// into an existing shell.
const STARTUP_GRACE: Duration = Duration::from_secs(3);

/// Test seam: override [`STARTUP_GRACE`] in milliseconds.
const ENV_STARTUP_GRACE_MS: &str = "PHUX_AGENT_STARTUP_GRACE_MS";

/// The effective startup grace, read once per process.
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

/// Something a pane's actor derived that only `ServerState` can act on,
/// drained by `runtime::client::spawn_agent_state_drain`. Not a wire type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentDetectEvent {
    /// Publish the privacy-bounded foreground-process observation.
    Occupant(identify::PaneOccupant),
    /// Write this record.
    State(AgentReport),
    /// The occupant changed (kind or pgid): correct the record in one write,
    /// landing on `unknown`. Best-effort (`try_send`), so the `State` path
    /// must reassert `kind` and `name` on every write anyway.
    Reidentified {
        /// The kind now occupying the pane.
        kind: String,
        /// The new occupant's manifest name.
        name: String,
    },
    /// The `phux-ask` title sentinel appeared, changed (`Some`), or cleared
    /// (`None`). Set and clear share this channel so they stay ordered.
    AskSentinel(Option<crate::agent_asked::AskedPayload>),
    /// The agent is gone; withdraw the record.
    Retract,
}

/// The result of one [`AgentDetector::tick`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DetectOutcome {
    /// Nothing to say. The overwhelmingly common outcome.
    Quiet,
    /// The derived tuple changed; publish it.
    Publish(AgentReport),
    /// A different occupant now owns the pane; correct the record.
    Reidentified {
        /// The kind now occupying the pane.
        kind: String,
        /// The new occupant's manifest name.
        name: String,
    },
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

/// One pane's detector, driven by the actor's timer (never by PTY bytes).
pub(crate) struct AgentDetector {
    rules: Rc<RuleSet>,
    /// The agent kind currently running, if any.
    identified: Option<String>,
    /// The identified occupant as `(pgid, leader start time)`: a restart in
    /// the same pane is a different agent, and re-anchors [`STARTUP_GRACE`].
    identified_occupant: Option<identify::Occupant>,
    /// The last foreground pgid any successful resolution reported. Between
    /// full rechecks a cheap `tcgetpgrp` is compared against it, and only a
    /// mismatch pays for the argv read. A recycled pgid looks identical, so
    /// the periodic full recheck still owns that case.
    last_pgid: Option<i32>,
    /// Consecutive confirmed-vacant observations. Kept here, not in the
    /// record, because a timestamp in the record would defeat write dedup.
    vacant_streak: u8,
    /// When identity is next re-derived.
    next_identify: Instant,
    /// Anchors [`IDENTIFY_ACQUIRE_WINDOW`].
    started: Instant,
    /// When the current agent was identified; anchors [`STARTUP_GRACE`].
    identified_at: Option<Instant>,
    /// The last tuple we published (the edge filter). Stale once something
    /// else writes the store; see [`Self::invalidate_published`].
    published: Option<AgentReport>,
    /// An in-flight `working -> idle` hold.
    pending_idle: Option<PendingIdle>,
    /// The last state we derived (used when a scan is skipped).
    current: Option<DetectedState>,
    /// Last pane-occupant record emitted, for independent edge filtering.
    published_occupant: Option<identify::PaneOccupant>,
    /// New occupant observation waiting for the actor to drain it.
    pending_occupant: Option<identify::PaneOccupant>,
    /// Latest hook edge received just before process identity resolved.
    pending_hook: Option<(DetectedState, Instant)>,
    /// Which evidence rung wrote [`Self::published`]. Only the hook and
    /// stream paths consult it; the screen is ranked by the live-session
    /// probe instead. `Screen` is the floor.
    published_source: crate::agent_state::EvidenceSource,
    /// "Does this pane own a live `AgentSession` child?" (ADR-0103 §5).
    /// `None` means no.
    live_session: Option<live_session::LiveSessionProbe>,
    /// The probe's previous answer, so the child ending is an edge.
    live_session_held: bool,
    /// The last state a live stream published, or `None` if it has said
    /// nothing or retracted. Decides whether a screen `idle` may publish
    /// (see [`Self::screen_may_publish`]).
    stream_state: Option<DetectedState>,
    cadence: Cadence,
    /// Test seam: where [`Self::reidentify`] gets identity from.
    #[cfg(test)]
    identity_source: IdentitySource,
}

/// Where [`AgentDetector::reidentify`] reads identity from in tests (a unit
/// test cannot create a real foreground process group).
#[cfg(test)]
#[derive(Debug, Clone)]
enum IdentitySource {
    /// Ask the kernel, as production does.
    Kernel,
    /// Report this, with no PTY in sight.
    Forced(identify::Occupancy),
}

impl std::fmt::Debug for AgentDetector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentDetector")
            .field("identified", &self.identified)
            .field("identified_occupant", &self.identified_occupant)
            .field("last_pgid", &self.last_pgid)
            .field("vacant_streak", &self.vacant_streak)
            .field("published", &self.published)
            .field("current", &self.current)
            .field("live_session_held", &self.live_session_held)
            .field("cadence", &self.cadence)
            .finish_non_exhaustive()
    }
}

impl AgentDetector {
    /// Inject hook evidence. The actor marks the grid dirty afterwards, so the
    /// next tick may supersede it with screen evidence.
    /// detector tick re-evaluates screen evidence and may supersede the hook.
    pub(crate) fn report_hook_state(
        &mut self,
        state: DetectedState,
        now: Instant,
    ) -> Option<AgentReport> {
        if !crate::agent_state::EvidenceSource::Hook.outranks(self.published_source) {
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
        self.published_source = crate::agent_state::EvidenceSource::Hook;
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
            self.published_source = crate::agent_state::EvidenceSource::Screen;
            self.pending_idle = None;
            return None;
        };
        self.published_source = crate::agent_state::EvidenceSource::Stream;
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
            published_source: crate::agent_state::EvidenceSource::Screen,
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
    fn live_session_active(&self) -> bool {
        self.live_session.as_ref().is_some_and(|probe| probe())
    }

    /// Whether a screen derivation may publish while a live session exists.
    const fn screen_may_publish(&self, derived: DetectedState, matched: bool) -> bool {
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

    /// Identity, cheap first: the periodic full recheck runs as always, and
    /// between rechecks a `tcgetpgrp` mismatch against [`Self::last_pgid`]
    /// triggers an early one. A recycled pgid is invisible here and left to
    /// the full recheck.
    fn maybe_identify(
        &mut self,
        now: Instant,
        master_fd: Option<RawFd>,
        pane_child_pid: Option<i32>,
    ) -> Option<DetectOutcome> {
        if now >= self.next_identify {
            return self.reidentify(now, master_fd, pane_child_pid);
        }
        let probed = self.resolve_pgid(master_fd);
        if let Some(pgid) = probed
            && Some(pgid) != self.last_pgid
        {
            // Cheap probe disagrees; re-derive now.
            return self.reidentify(now, master_fd, pane_child_pid);
        }
        None
    }

    /// Re-derive identity in full (pgid and argv). `Some` means the identity
    /// step alone resolves the tick.
    fn reidentify(
        &mut self,
        now: Instant,
        master_fd: Option<RawFd>,
        pane_child_pid: Option<i32>,
    ) -> Option<DetectOutcome> {
        let (found, occupant) = self.resolve_identity(master_fd, pane_child_pid);
        if let Some(occupant) = occupant
            && self.published_occupant.as_ref() != Some(&occupant)
        {
            self.pending_occupant = Some(occupant);
        }
        self.apply_identity(now, found)
    }

    /// Ask the kernel who owns the PTY's foreground process group.
    #[cfg(not(test))]
    fn resolve_identity(
        &self,
        master_fd: Option<RawFd>,
        pane_child_pid: Option<i32>,
    ) -> (identify::Occupancy, Option<identify::PaneOccupant>) {
        identify::foreground_observation(master_fd, pane_child_pid, &self.rules)
    }

    /// As above, honouring the [`IdentitySource`] test seam.
    #[cfg(test)]
    fn resolve_identity(
        &self,
        master_fd: Option<RawFd>,
        pane_child_pid: Option<i32>,
    ) -> (identify::Occupancy, Option<identify::PaneOccupant>) {
        match &self.identity_source {
            IdentitySource::Kernel => {
                identify::foreground_observation(master_fd, pane_child_pid, &self.rules)
            }
            IdentitySource::Forced(occupancy) => (occupancy.clone(), None),
        }
    }

    /// Take the independently edge-filtered pane-occupant observation.
    pub(crate) const fn take_occupant_update(&mut self) -> Option<identify::PaneOccupant> {
        self.pending_occupant.take()
    }

    /// Advance the occupant edge filter only after the actor enqueues it.
    pub(crate) fn occupant_update_sent(&mut self, occupant: identify::PaneOccupant) {
        self.published_occupant = Some(occupant);
    }

    /// Preserve a failed best-effort enqueue for the next detector tick.
    pub(crate) fn retry_occupant_update(&mut self, occupant: identify::PaneOccupant) {
        self.pending_occupant = Some(occupant);
    }

    /// The cheap half: foreground pgid only.
    #[cfg(not(test))]
    #[allow(
        clippy::unused_self,
        reason = "the #[cfg(test)] sibling reads self.identity_source; matching signatures keep the call site uniform across both"
    )]
    fn resolve_pgid(&self, master_fd: Option<RawFd>) -> Option<i32> {
        identify::foreground_pgid(master_fd)
    }

    /// As above, honouring the [`IdentitySource`] test seam.
    #[cfg(test)]
    fn resolve_pgid(&self, master_fd: Option<RawFd>) -> Option<i32> {
        match &self.identity_source {
            IdentitySource::Kernel => identify::foreground_pgid(master_fd),
            IdentitySource::Forced(occupancy) => occupancy.pgid(),
        }
    }

    /// The manifest's name for `kind`, or the slug itself.
    fn manifest_name(&self, kind: &str) -> String {
        self.rules
            .manifest(kind)
            .map_or_else(|| kind.to_owned(), |m| m.name.clone())
    }

    /// Everything after the kernel answered who owns the foreground group.
    /// The transition table, in full:
    ///
    /// | observation | prior identity | action |
    /// |---|---|---|
    /// | `Unresolved` | any | HOLD. Touch nothing. `Quiet`. |
    /// | `Vacant` | none | `Quiet`. |
    /// | `Vacant` | some, streak < [`VACANT_CONFIRMATIONS`] | `Quiet`. |
    /// | `Vacant` | some, streak reached | drop identity, `Retract`. |
    /// | `Agent` | none | acquire; `Quiet` through the startup grace. |
    /// | `Agent`, different kind OR different pgid | some | re-acquire **and** `Reidentified`. |
    /// | `Agent`, same kind, same pgid | some | fall through to the screen. |
    ///
    /// At most one event per tick: `Retract` and `Reidentified` short-circuit,
    /// and the re-anchored [`STARTUP_GRACE`] keeps a new occupant's `State`
    /// strictly after its correction.
    fn apply_identity(
        &mut self,
        now: Instant,
        occupancy: identify::Occupancy,
    ) -> Option<DetectOutcome> {
        use identify::Occupancy;

        // Remember this full resolution's pgid for the cheap probe
        // (`Unresolved` carries none).
        if let Some(pgid) = occupancy.pgid() {
            self.last_pgid = Some(pgid);
        }

        let acquiring = !matches!(occupancy, Occupancy::Agent { .. })
            && now < self.started + IDENTIFY_ACQUIRE_WINDOW;
        self.next_identify = now
            + if acquiring {
                IDENTIFY_ACQUIRE_POLL
            } else {
                identify_recheck()
            };

        match occupancy {
            // A failed query is not an observation: hold, and never advance
            // the vacancy streak.
            Occupancy::Unresolved => {
                trace!("agent-detect: occupancy unresolved; holding");
                Some(DetectOutcome::Quiet)
            }
            Occupancy::Vacant { .. } => Some(self.apply_vacancy()),
            Occupancy::Agent { kind, occupant } => self.apply_agent(now, kind, occupant),
        }
    }

    /// A *successful* observation that no known agent owns the pane.
    fn apply_vacancy(&mut self) -> DetectOutcome {
        if self.identified.is_none() {
            self.vacant_streak = 0;
            return DetectOutcome::Quiet;
        }
        self.vacant_streak = self.vacant_streak.saturating_add(1);
        if self.vacant_streak < VACANT_CONFIRMATIONS {
            trace!(
                streak = self.vacant_streak,
                "agent-detect: vacant, not yet confirmed",
            );
            return DetectOutcome::Quiet;
        }

        // Confirmed gone: retract.
        self.identified = None;
        self.identified_occupant = None;
        self.identified_at = None;
        self.pending_idle = None;
        self.current = None;
        self.pending_hook = None;
        self.cadence = Cadence::Unidentified;
        self.vacant_streak = 0;
        // Gate on having had an identity, not on `published`, which may be
        // briefly `None` after any metadata write.
        self.published = None;
        trace!("agent-detect: occupant confirmed gone; retracting");
        DetectOutcome::Retract
    }

    /// A *successful* observation that `kind` (running as `occupant`) owns the
    /// pane.
    fn apply_agent(
        &mut self,
        now: Instant,
        kind: String,
        occupant: identify::Occupant,
    ) -> Option<DetectOutcome> {
        self.vacant_streak = 0;

        let same_kind = self.identified.as_deref() == Some(kind.as_str());
        // A `None` prior occupant heals silently rather than counting as a
        // change.
        let same_occupant = same_kind
            && self
                .identified_occupant
                .is_none_or(|prior| prior.same(occupant));
        if same_occupant {
            // Refresh, but never downgrade a known start time to `None`.
            self.identified_occupant = Some(match self.identified_occupant {
                Some(prior) if occupant.started.is_none() => {
                    identify::Occupant::new(occupant.pgid, prior.started)
                }
                _ => occupant,
            });
            return None;
        }

        let replaced = self.identified.is_some();
        trace!(%kind, pgid = occupant.pgid, replaced, "agent-detect: identified");
        self.identified = Some(kind.clone());
        self.identified_occupant = Some(occupant);
        // Grace restarts here, including for a same-kind restart.
        self.identified_at = Some(now);
        // A different occupant: nothing previously derived applies.
        self.published = None;
        self.pending_idle = None;
        self.current = None;
        self.cadence = Cadence::Identified;

        // Acquiring an empty pane is not a correction.
        if !replaced {
            return None;
        }
        let name = self.manifest_name(&kind);
        Some(DetectOutcome::Reidentified { kind, name })
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

        // Ambiguous: look again, fast.
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

    /// Test seam: the state this detector last published.
    #[cfg(test)]
    pub(crate) fn published_state(&self) -> Option<DetectedState> {
        self.published.as_ref().map(|report| report.state)
    }
    #[cfg(test)]
    /// Test seam: force an identity without a live PTY.
    #[cfg(test)]
    pub(crate) fn force_identity(&mut self, kind: &str, now: Instant) {
        self.identified = Some(kind.to_owned());
        self.identified_at = Some(now);
        self.cadence = Cadence::Identified;
        // Push the next identity poll out; there is no PTY to read.
        self.next_identify = now + Duration::from_secs(3600);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use super::identify::{Occupancy, Occupant};
    use super::rules::{ManifestSpec, RuleSet};
    use super::{
        AgentDetector, DetectOutcome, DetectedState, HOOK_IDENTITY_GRACE, IDLE_CONFIRMATIONS,
        STARTUP_GRACE, TICK_CONFIRMING, TICK_IDENTIFIED, VACANT_CONFIRMATIONS,
    };

    /// A manifest exercising every arm of the hysteresis machine.
    const MANIFEST: &str = r#"
kind = "t"
name = "t-agent"
binaries = ["t"]

[[rules]]
id = "working"
state = "working"
priority = 50
region = "viewport"
match = { contains = "WORKING" }

[[rules]]
id = "blocked"
state = "blocked"
priority = 80
region = "viewport"
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

    /// A second kind, so an occupant change is expressible.
    const MANIFEST_OTHER: &str = r#"
kind = "u"
name = "u-agent"
binaries = ["u"]

[[rules]]
id = "working"
state = "working"
priority = 50
region = "viewport"
match = { contains = "WORKING" }
"#;

    struct Harness {
        detector: AgentDetector,
        now: Instant,
        /// The actor's `agent_dirty_since_detect` flag.
        dirty: bool,
        /// The fake `AgentSession` child behind the live-session probe. Wired
        /// for every harness, so `false` also proves a quiet probe is inert.
        live_session: Rc<std::cell::Cell<bool>>,
    }

    impl Harness {
        fn new() -> Self {
            let mut h = Self::unidentified();
            let now = h.now;
            h.detector.force_identity("t", now);
            h
        }

        /// A pane that is still just a shell.
        fn unidentified() -> Self {
            let spec: ManifestSpec = toml::from_str(MANIFEST).expect("manifest parses");
            let other: ManifestSpec = toml::from_str(MANIFEST_OTHER).expect("manifest parses");
            let mut set = RuleSet::default();
            set.install(spec).expect("compiles");
            set.install(other).expect("compiles");
            let now = Instant::now();
            let live_session = Rc::new(std::cell::Cell::new(false));
            let mut detector = AgentDetector::new(Rc::new(set), now);
            let probe = Rc::clone(&live_session);
            detector.set_live_session_probe(Rc::new(move || probe.get()));
            Self {
                detector,
                now,
                dirty: false,
                live_session,
            }
        }

        fn past_grace(mut self) -> Self {
            self.now += STARTUP_GRACE + Duration::from_millis(1);
            self
        }

        /// Replay the actor's real `detect_tick` ordering: `wants_screen` is
        /// asked before identity resolves, and the dirty flag is consumed only
        /// when a scan happens.
        fn actor_tick(&mut self, screen: &str) -> DetectOutcome {
            self.now += self.detector.interval();
            let lines = [screen.to_owned()];
            let scan = self.detector.wants_screen(self.dirty);
            if scan {
                self.dirty = false;
            }
            self.detector
                .tick(self.now, None, "", "", scan.then_some(&lines[..]))
        }

        /// A human launches an agent at the shell prompt; it paints.
        fn launch_agent(&mut self, kind: &str) {
            self.occupy(agent(kind, 100));
            self.dirty = true;
        }

        /// Set what the kernel reports about the foreground group.
        fn occupy(&mut self, occupancy: Occupancy) {
            self.detector.identity_source = super::IdentitySource::Forced(occupancy);
        }

        /// Make the next tick's identity poll due.
        fn poll_identity_now(&mut self) {
            self.detector.next_identify = self.now;
        }

        /// A tick whose identity poll runs, with a `WORKING` screen.
        fn identity_tick(&mut self) -> DetectOutcome {
            self.poll_identity_now();
            self.tick("WORKING")
        }

        /// Advance by the detector's interval and feed it a screen.
        fn tick(&mut self, screen: &str) -> DetectOutcome {
            self.now += self.detector.interval();
            let lines = vec![screen.to_owned()];
            self.detector.tick(self.now, None, "", "", Some(&lines))
        }

        /// Tick with the scan skipped (the cheap steady state).
        fn tick_no_scan(&mut self) -> DetectOutcome {
            self.now += self.detector.interval();
            self.detector.tick(self.now, None, "", "", None)
        }

        fn state(&self) -> Option<DetectedState> {
            self.detector.published.as_ref().map(|r| r.state)
        }

        fn session_open(&self) {
            self.live_session.set(true);
        }

        fn session_end(&self) {
            self.live_session.set(false);
        }

        /// The live child's stream publishes `state` at rank `Stream`.
        fn stream_says(&mut self, state: DetectedState) {
            let now = self.now;
            self.detector.report_stream_state(Some(state), now);
        }
    }

    /// A live agent occupant; the start time is derived from the pgid.
    fn agent(kind: &str, pgid: i32) -> Occupancy {
        let started = u64::try_from(pgid).unwrap_or(0).saturating_mul(7);
        agent_started(kind, pgid, Some(started))
    }

    /// As [`agent`], but stating the start time outright.
    fn agent_started(kind: &str, pgid: i32, started: Option<u64>) -> Occupancy {
        Occupancy::Agent {
            kind: kind.to_owned(),
            occupant: Occupant::new(pgid, started),
        }
    }

    fn published(outcome: &DetectOutcome) -> DetectedState {
        match outcome {
            DetectOutcome::Publish(report) => report.state,
            other => panic!("expected a publish, got {other:?}"),
        }
    }

    #[test]
    fn attention_states_publish_on_the_first_tick() {
        for (screen, state) in [
            ("BLOCKED", DetectedState::Blocked),
            ("WORKING", DetectedState::Working),
            ("DONE", DetectedState::Done),
        ] {
            let mut h = Harness::new().past_grace();
            assert_eq!(published(&h.tick(screen)), state, "{screen}");
        }
    }

    #[test]
    fn hook_done_is_immediate_and_the_next_screen_can_supersede_it() {
        let mut h = Harness::new().past_grace();
        let report = h
            .detector
            .report_hook_state(DetectedState::Done, h.now)
            .expect("new edge");
        assert_eq!(report.state, DetectedState::Done);
        assert_eq!(h.state(), Some(DetectedState::Done));
        assert_eq!(
            published(&h.tick("WORKING")),
            DetectedState::Working,
            "hook evidence must not stand screen derivation down"
        );
    }

    #[test]
    fn hook_state_waits_for_the_first_identity_poll() {
        let mut h = Harness::unidentified();
        assert_eq!(
            h.detector.report_hook_state(DetectedState::Done, h.now),
            None
        );
        h.launch_agent("t");
        assert_eq!(published(&h.actor_tick("WORKING")), DetectedState::Done);
    }

    #[test]
    fn pre_identity_hook_expires_before_an_unrelated_future_occupant() {
        let mut h = Harness::unidentified();
        assert_eq!(
            h.detector.report_hook_state(DetectedState::Done, h.now),
            None
        );
        h.now += HOOK_IDENTITY_GRACE + Duration::from_millis(1);
        h.launch_agent("t");
        assert!(matches!(h.actor_tick("WORKING"), DetectOutcome::Quiet));
        assert_eq!(h.state(), None);
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

    /// Ambiguous `working -> idle`: held, then released.
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

    /// The hold ticks at the fast confirming cadence.
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

    /// Positive idle evidence bypasses the hold.
    #[test]
    fn visible_idle_bypasses_the_hold() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
        let out = h.tick("IDLE");
        assert_eq!(published(&out), DetectedState::Idle, "no debounce");
        assert_eq!(h.detector.interval(), TICK_IDENTIFIED);
    }

    /// The hold is capped in wall-clock time.
    #[test]
    fn the_hold_is_capped_in_wall_clock_time() {
        let mut h = Harness::new().past_grace();
        h.tick("WORKING");
        // One ambiguous tick to open the hold ...
        assert_eq!(h.tick("nothing"), DetectOutcome::Quiet);
        // Past the cap, the next tick releases.
        h.now += super::IDLE_HOLD_CAP;
        let lines = vec!["nothing".to_owned()];
        let out = h.detector.tick(h.now, None, "", "", Some(&lines));
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

    /// Idle from a non-working state is not debounced.
    #[test]
    fn idle_from_blocked_is_not_debounced() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("BLOCKED")), DetectedState::Blocked);
        let out = h.tick("nothing matches");
        assert_eq!(published(&out), DetectedState::Idle);
    }

    /// A pager carries no information: freeze.
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

    /// A freeze rule outranks a matching blocked rule.
    #[test]
    fn freeze_outranks_every_state_bearing_rule() {
        let mut h = Harness::new().past_grace();
        assert_eq!(h.tick("PAGER and BLOCKED"), DetectOutcome::Quiet);
        assert_eq!(h.state(), None);
    }

    /// The fail-safe: no matching rule means idle, never blocked.
    #[test]
    fn an_identified_agent_with_no_matching_rule_is_idle_never_blocked() {
        let mut h = Harness::new().past_grace();
        let out = h.tick("total gibberish that matches nothing");
        assert_eq!(published(&out), DetectedState::Idle);
        assert_ne!(h.state(), Some(DetectedState::Blocked));
    }

    /// A long `working` run produces exactly one write.
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

    /// A splash screen containing a blocked word does not publish in grace.
    #[test]
    fn the_startup_grace_suppresses_publication() {
        let mut h = Harness::new();
        // `elapsed` tracks where the next tick lands, never the boundary.
        let mut elapsed = TICK_IDENTIFIED;
        while elapsed < STARTUP_GRACE {
            assert_eq!(h.tick("BLOCKED"), DetectOutcome::Quiet, "silent in grace");
            elapsed += TICK_IDENTIFIED;
        }
        assert_eq!(h.state(), None);
        // Once the grace expires, the very same screen publishes.
        h.now += STARTUP_GRACE;
        let lines = vec!["BLOCKED".to_owned()];
        let out = h.detector.tick(h.now, None, "", "", Some(&lines));
        assert_eq!(published(&out), DetectedState::Blocked);
    }

    /// With the scan skipped, the detector holds and says nothing.
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

    /// A static `blocked` prompt with a clean grid is not rescanned.
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

    /// A freshly identified pane scans even with a clean grid.
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

    /// An unidentified pane never publishes or retracts.
    #[test]
    fn an_unidentified_pane_is_silent() {
        let spec: ManifestSpec = toml::from_str(MANIFEST).expect("parses");
        let mut set = RuleSet::default();
        set.install(spec).expect("compiles");
        let now = Instant::now();
        let mut detector = AgentDetector::new(Rc::new(set), now);
        for i in 0..5 {
            let at = now + Duration::from_secs(i * 6);
            assert_eq!(detector.tick(at, None, "", "", None), DetectOutcome::Quiet);
        }
    }

    // --- occupancy: departure needs evidence, and evidence needs confirming -

    /// An observed non-agent occupant retracts the record.
    #[test]
    fn losing_the_occupant_retracts_a_published_record() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);

        // The agent exits back to its shell.
        h.occupy(Occupancy::Vacant { pgid: 100 });
        for i in 1..VACANT_CONFIRMATIONS {
            assert_eq!(
                h.identity_tick(),
                DetectOutcome::Quiet,
                "vacancy {i} is not yet confirmed",
            );
            assert_eq!(h.state(), Some(DetectedState::Working), "badge still held");
        }
        assert_eq!(h.identity_tick(), DetectOutcome::Retract);
        assert_eq!(h.state(), None, "the badge is gone");

        // And it retracts exactly once.
        assert_eq!(h.identity_tick(), DetectOutcome::Quiet);
    }

    /// A long vacancy is one retraction, then silence.
    #[test]
    fn a_vacant_pane_retracts_once_and_then_is_silent() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
        h.occupy(Occupancy::Vacant { pgid: 100 });

        let mut retracts = 0;
        let mut quiets = 0;
        for _ in 0..20 {
            match h.identity_tick() {
                DetectOutcome::Retract => retracts += 1,
                DetectOutcome::Quiet => quiets += 1,
                other => panic!("a vacant pane must never publish or correct: {other:?}"),
            }
        }
        assert_eq!(retracts, 1, "exactly one retraction, ever");
        assert_eq!(quiets, 19);
    }

    /// Unanswerable queries are not evidence: twenty failures accumulate
    /// into nothing.
    #[test]
    fn an_unresolved_occupancy_never_retracts() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
        h.occupy(Occupancy::Unresolved);

        for i in 0..20 {
            assert_eq!(
                h.identity_tick(),
                DetectOutcome::Quiet,
                "unanswered query {i} must change nothing",
            );
        }
        assert_eq!(
            h.detector.identified.as_deref(),
            Some("t"),
            "the pane is still believed to host the agent it hosted",
        );
        assert_eq!(h.state(), Some(DetectedState::Working), "badge held");

        // The failures contributed nothing to the vacancy streak.
        h.occupy(Occupancy::Vacant { pgid: 100 });
        assert_eq!(
            h.identity_tick(),
            DetectOutcome::Quiet,
            "streak starts at 0"
        );
        assert_eq!(h.identity_tick(), DetectOutcome::Retract);
    }

    /// One vacant observation (a brief subprocess) does not retract.
    #[test]
    fn a_single_vacant_observation_does_not_retract() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);

        h.occupy(Occupancy::Vacant { pgid: 100 });
        assert_eq!(h.identity_tick(), DetectOutcome::Quiet);
        // The subprocess exits and the agent is back in the foreground.
        h.occupy(agent("t", 100));
        assert_eq!(h.identity_tick(), DetectOutcome::Quiet, "nothing happened");
        assert_eq!(h.state(), Some(DetectedState::Working), "badge untouched");

        // The streak reset too.
        h.occupy(Occupancy::Vacant { pgid: 100 });
        assert_eq!(h.identity_tick(), DetectOutcome::Quiet);
    }

    // --- the occupant changed (phux-w7z2.27) --------------------------------

    /// Acquire through the real identity path and publish `working`.
    fn occupied(kind: &str, pgid: i32) -> Harness {
        let mut h = Harness::unidentified();
        h.occupy(agent(kind, pgid));
        assert_eq!(h.identity_tick(), DetectOutcome::Quiet, "acquiring");
        h.now += STARTUP_GRACE + Duration::from_millis(1);
        assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
        h
    }

    /// A different kind in the same pane is corrected, not left beside a
    /// stale `kind`.
    #[test]
    fn a_different_kind_in_the_same_pane_is_a_correction() {
        let mut h = occupied("t", 100);

        h.occupy(agent("u", 200));
        assert_eq!(
            h.identity_tick(),
            DetectOutcome::Reidentified {
                kind: "u".to_owned(),
                name: "u-agent".to_owned(),
            },
            "the pane's occupant changed and someone has to be told",
        );
    }

    /// A correction is never a retraction.
    #[test]
    fn a_kind_change_never_retracts() {
        let mut h = occupied("t", 100);
        h.occupy(agent("u", 200));
        assert!(
            !matches!(h.identity_tick(), DetectOutcome::Retract),
            "the pane is occupied; there is nothing to retract",
        );
    }

    /// A same-kind restart (new pgid) is a new occupant.
    #[test]
    fn a_same_kind_restart_is_a_new_occupant() {
        let mut h = occupied("t", 100);

        h.occupy(agent("t", 271));
        assert_eq!(
            h.identity_tick(),
            DetectOutcome::Reidentified {
                kind: "t".to_owned(),
                name: "t-agent".to_owned(),
            },
            "a restart is an identity change, whatever the binary is called",
        );
    }

    /// The same occupant seen again is not an event.
    #[test]
    fn the_same_occupant_seen_again_is_not_an_event() {
        let mut h = occupied("t", 100);
        for _ in 0..20 {
            assert_eq!(
                h.identity_tick(),
                DetectOutcome::Quiet,
                "the same agent, still there, is not news",
            );
        }
        assert_eq!(h.state(), Some(DetectedState::Working));
    }

    // --- pid reuse (phux-w7z2.43) -------------------------------------------

    /// A recycled pgid with a new start time is a new occupant.
    #[test]
    fn a_recycled_pgid_is_a_new_occupant_not_the_old_one() {
        let mut h = occupied("t", 100);

        // Same kind, same id, different process.
        h.occupy(agent_started("t", 100, Some(999_999)));
        assert_eq!(
            h.identity_tick(),
            DetectOutcome::Reidentified {
                kind: "t".to_owned(),
                name: "t-agent".to_owned(),
            },
            "the id was recycled; the start time is what says so",
        );
    }

    /// A stable start time never manufactures a restart.
    #[test]
    fn a_stable_start_time_never_manufactures_a_restart() {
        let mut h = occupied("t", 100);
        for _ in 0..20 {
            assert_eq!(
                h.identity_tick(),
                DetectOutcome::Quiet,
                "same pgid, same start time: nothing happened",
            );
        }
        assert_eq!(h.state(), Some(DetectedState::Working), "badge untouched");
    }

    /// Without start times, comparison degrades to pgids, not "all new".
    #[test]
    fn an_unavailable_start_time_degrades_to_comparing_pgids() {
        let mut h = Harness::unidentified();
        h.occupy(agent_started("t", 100, None));
        assert_eq!(h.identity_tick(), DetectOutcome::Quiet, "acquiring");
        h.now += STARTUP_GRACE + Duration::from_millis(1);
        assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);

        for _ in 0..20 {
            assert_eq!(
                h.identity_tick(),
                DetectOutcome::Quiet,
                "no start time anywhere: fall back to the pgid, stay silent",
            );
        }
        // ... and the pgid alone still catches the restarts it always caught.
        h.occupy(agent_started("t", 271, None));
        assert!(matches!(
            h.identity_tick(),
            DetectOutcome::Reidentified { .. }
        ));
    }

    /// A transiently unreadable start time does not erase the known one.
    #[test]
    fn a_transiently_unreadable_start_time_does_not_erase_the_one_we_have() {
        let mut h = occupied("t", 100);

        // Same pgid, failed start-time reads: same occupant, nothing emitted.
        for _ in 0..3 {
            h.occupy(agent_started("t", 100, None));
            assert_eq!(h.identity_tick(), DetectOutcome::Quiet);
        }

        // The original start time still catches a later recycle.
        h.occupy(agent_started("t", 100, Some(999_999)));
        assert_eq!(
            h.identity_tick(),
            DetectOutcome::Reidentified {
                kind: "t".to_owned(),
                name: "t-agent".to_owned(),
            },
            "the remembered start time survived the failed queries",
        );
    }

    // --- the cheap pgid probe (phux-w7z2.50) --------------------------------

    /// A pgid change is caught by the cheap probe on an ordinary tick.
    #[test]
    fn a_pgid_change_is_caught_by_the_ordinary_cadence_not_just_the_recheck() {
        let mut h = occupied("t", 100);

        // The full recheck is not due yet, so only the cheap probe can see it.
        h.occupy(agent("u", 200));
        assert_eq!(
            h.tick("WORKING"),
            DetectOutcome::Reidentified {
                kind: "u".to_owned(),
                name: "u-agent".to_owned(),
            },
            "the cheap pgid-only probe must not wait out the full recheck \
             for a change it can see for the price of one ioctl",
        );
    }

    /// A recycled pgid is invisible to the cheap probe but still caught on
    /// the full recheck.
    #[test]
    fn a_recycled_pgid_is_invisible_to_the_cheap_probe_but_still_caught_on_recheck() {
        let mut h = occupied("t", 100);

        h.occupy(agent_started("t", 100, Some(999_999)));

        // Same pgid every cheap probe: stay quiet.
        for i in 0..5 {
            assert_eq!(
                h.tick("WORKING"),
                DetectOutcome::Quiet,
                "tick {i}: the cheap probe cannot afford to notice a same-pgid \
                 recycle, and must not guess",
            );
            assert_eq!(
                h.detector.identified_occupant,
                Some(Occupant::new(100, Some(700))),
                "tick {i}: still believed to be the ORIGINAL process",
            );
        }

        assert_eq!(
            h.identity_tick(),
            DetectOutcome::Reidentified {
                kind: "t".to_owned(),
                name: "t-agent".to_owned(),
            },
            "the full recheck still closes the .43 hole the cheap probe cannot",
        );
    }

    /// An unresolvable cheap probe holds rather than guessing.
    #[test]
    fn an_unresolvable_cheap_probe_holds_rather_than_guessing() {
        let mut h = occupied("t", 100);
        h.occupy(Occupancy::Unresolved);
        for _ in 0..5 {
            assert_eq!(h.tick("WORKING"), DetectOutcome::Quiet);
        }
        assert_eq!(
            h.detector.identified.as_deref(),
            Some("t"),
            "still believed present"
        );
        assert_eq!(h.state(), Some(DetectedState::Working), "badge untouched");
    }

    /// A new occupant gets its own startup grace.
    #[test]
    fn a_new_occupant_re_anchors_the_startup_grace() {
        let mut h = occupied("t", 100);
        h.occupy(agent("u", 200));
        assert!(matches!(
            h.identity_tick(),
            DetectOutcome::Reidentified { .. }
        ));

        // Its splash screen paints something a rule matches. Silence.
        let mut elapsed = TICK_IDENTIFIED;
        while elapsed < STARTUP_GRACE {
            assert_eq!(
                h.tick("WORKING"),
                DetectOutcome::Quiet,
                "the new occupant is still painting",
            );
            elapsed += TICK_IDENTIFIED;
        }
        assert_eq!(h.state(), None, "nothing published between the two agents");

        // And then the truth, attributed to the RIGHT process.
        h.now += STARTUP_GRACE;
        let lines = vec!["WORKING".to_owned()];
        match h.detector.tick(h.now, None, "", "", Some(&lines)) {
            DetectOutcome::Publish(report) => {
                assert_eq!(report.kind, "u");
                assert_eq!(report.name, "u-agent");
                assert_eq!(report.state, DetectedState::Working);
            }
            other => panic!("expected a publish for the new occupant, got {other:?}"),
        }
    }

    /// A correction is alone in its tick, never with a state.
    #[test]
    fn a_correction_short_circuits_its_own_tick() {
        let mut h = occupied("t", 100);
        h.occupy(agent("u", 200));
        assert!(matches!(
            h.identity_tick(),
            DetectOutcome::Reidentified { .. }
        ));
        assert_eq!(
            h.detector.published, None,
            "nothing about the new occupant has been published yet",
        );
    }

    // --- the mid-pane agent launch (the dominant interactive flow) ---------

    /// An agent launched at a shell that paints a dialog and goes silent must
    /// not latch to `idle`: the identifying tick reads no screen and must
    /// neither publish nor consume the dirty flag.
    #[test]
    fn a_mid_pane_agent_launch_does_not_latch_to_idle() {
        let mut h = Harness::unidentified();

        // Two minutes of plain shell. The acquire window lapsed long ago.
        for _ in 0..240 {
            assert_eq!(h.actor_tick("$ "), DetectOutcome::Quiet);
        }

        h.launch_agent("t");
        assert!(
            !h.detector.wants_screen(h.dirty),
            "unidentified: there is nothing to derive against, so no scan",
        );

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

    /// A screen-less tick with nothing derived holds instead of guessing.
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

    /// The startup grace is anchored at identification, not pane creation.
    #[test]
    fn the_startup_grace_is_anchored_at_identification_not_pane_creation() {
        let mut h = Harness::unidentified();
        h.now += Duration::from_secs(600);
        h.launch_agent("t");

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

    /// After the store changes, an unchanged state is republished once.
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

    /// The same without a scan (an idle agent's clean grid).
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

    /// A pager during an idle hold drops the hold and the fast cadence.
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

        // Closing the pager restarts the hold from scratch.
        for i in 1..IDLE_CONFIRMATIONS {
            assert_eq!(
                h.tick("nothing matches"),
                DetectOutcome::Quiet,
                "confirmation {i} of the restarted hold",
            );
        }
        assert_eq!(published(&h.tick("nothing matches")), DetectedState::Idle);
    }

    /// Title rules outrank screen rules, end to end.
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
        let out = detector.tick(at, None, "busy", "", Some(&lines));
        assert_eq!(published(&out), DetectedState::Working);
    }

    /// With a live session the screen cannot publish a lifecycle state, not
    /// even a matched `blocked`.
    #[test]
    fn a_live_agent_session_suppresses_a_contradicting_screen_derivation() {
        let mut h = Harness::new().past_grace();
        h.session_open();
        assert_eq!(
            h.tick("BLOCKED"),
            DetectOutcome::Quiet,
            "the screen must not publish over a live session's stream",
        );
        assert_eq!(h.state(), None, "and must not advance its own edge filter");
        assert_eq!(h.tick("WORKING"), DetectOutcome::Quiet, "nor a second one");
        assert_eq!(h.state(), None);
    }

    /// A session that has said nothing leaves idle (and identity) to the
    /// screen.
    #[test]
    fn a_live_agent_session_leaves_the_idle_confirmation_to_the_screen() {
        let mut h = Harness::new().past_grace();
        h.session_open();
        assert_eq!(
            published(&h.tick("a quiet prompt")),
            DetectedState::Idle,
            "idle is detector-owned while a session is live but silent",
        );
    }

    /// The fail-safe idle does not overwrite a stream's `done`.
    #[test]
    fn the_fail_safe_idle_does_not_overwrite_a_streams_done() {
        let mut h = Harness::new().past_grace();
        h.session_open();
        h.stream_says(DetectedState::Done);
        for _ in 0..IDLE_CONFIRMATIONS.saturating_add(2) {
            assert_eq!(h.tick("a screen no rule matches"), DetectOutcome::Quiet);
        }
        assert_eq!(h.state(), Some(DetectedState::Done));
    }

    /// The fail-safe idle does not overwrite a live stream's `working`.
    #[test]
    fn the_fail_safe_idle_does_not_overwrite_a_live_stream() {
        let mut h = Harness::new().past_grace();
        h.session_open();
        h.stream_says(DetectedState::Working);
        assert_eq!(h.state(), Some(DetectedState::Working));

        for _ in 0..IDLE_CONFIRMATIONS.saturating_add(2) {
            assert_eq!(
                h.tick("a screen no rule matches"),
                DetectOutcome::Quiet,
                "no-rule-matched is not evidence that the turn ended",
            );
        }
        assert_eq!(
            h.state(),
            Some(DetectedState::Working),
            "the stream's state survives the screen tick",
        );
    }

    /// Even a positive idle does not talk over a stream still `working`.
    #[test]
    fn a_positive_idle_does_not_overwrite_a_stream_still_working() {
        let mut h = Harness::new().past_grace();
        h.session_open();
        h.stream_says(DetectedState::Working);
        assert_eq!(h.tick("IDLE"), DetectOutcome::Quiet);
        assert_eq!(h.state(), Some(DetectedState::Working));

        h.stream_says(DetectedState::Blocked);
        assert_eq!(h.tick("IDLE"), DetectOutcome::Quiet, "nor a blocked one");
        assert_eq!(h.state(), Some(DetectedState::Blocked));
    }

    /// After the stream says `stop`, a positive idle publishes.
    #[test]
    fn a_positive_idle_publishes_once_the_stream_has_stopped() {
        let mut h = Harness::new().past_grace();
        h.session_open();
        h.stream_says(DetectedState::Working);
        h.stream_says(DetectedState::Done);
        assert_eq!(
            published(&h.tick("IDLE")),
            DetectedState::Idle,
            "after `stop` the screen owns the idle confirmation again",
        );
    }

    /// A retracted stream hands idle straight back to the screen.
    #[test]
    fn a_retracted_stream_hands_the_idle_confirmation_straight_back() {
        let mut h = Harness::new().past_grace();
        h.session_open();
        h.stream_says(DetectedState::Working);
        let now = h.now;
        h.detector.report_stream_state(None, now);
        assert_eq!(published(&h.tick("IDLE")), DetectedState::Idle);
    }

    /// Confirmed departure still retracts with a live session (`kill -9`
    /// runs no `session_end`).
    #[test]
    fn a_live_agent_session_still_retracts_a_departed_agent() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("WORKING")), DetectedState::Working);
        h.session_open();

        h.occupy(Occupancy::Vacant { pgid: 100 });
        for _ in 1..VACANT_CONFIRMATIONS {
            assert_eq!(h.identity_tick(), DetectOutcome::Quiet);
        }
        assert_eq!(
            h.identity_tick(),
            DetectOutcome::Retract,
            "a live session does not make a dead agent's badge unretractable",
        );
        assert_eq!(h.state(), None);
    }

    /// After `session_end` the screen resumes and reasserts once.
    #[test]
    fn the_screen_scrape_resumes_and_reasserts_once_when_the_session_ends() {
        let mut h = Harness::new().past_grace();
        assert_eq!(published(&h.tick("BLOCKED")), DetectedState::Blocked);

        h.session_open();
        assert_eq!(h.tick("BLOCKED"), DetectOutcome::Quiet, "suppressed");

        h.session_end();
        assert_eq!(
            published(&h.tick("BLOCKED")),
            DetectedState::Blocked,
            "the resumed scrape reasserts what it can now see",
        );
        assert_eq!(
            h.tick("BLOCKED"),
            DetectOutcome::Quiet,
            "exactly once: the edge filter is armed again, not disabled",
        );
    }

    /// Hook evidence is not suppressed by a live session.
    #[test]
    fn hook_evidence_is_not_suppressed_by_a_live_session() {
        let mut h = Harness::new().past_grace();
        h.session_open();
        let report = h
            .detector
            .report_hook_state(DetectedState::Blocked, h.now)
            .expect("a hook edge publishes");
        assert_eq!(report.state, DetectedState::Blocked);
    }
}
