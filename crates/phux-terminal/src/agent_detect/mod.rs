//! Level-triggered agent-state detector (ADR-0046).
//!
//! Re-deriving state from the screen every tick is self-healing, unlike
//! edge-triggered hooks, where one missed transition lies forever.
//! `AgentDetector::tick` does, in order:
//!
//! 1. **Identify** the agent from the PTY's foreground process group.
//! 2. **Grace**: publish nothing for `STARTUP_GRACE` after identification,
//!    so a splash screen never flashes `blocked`.
//! 3. **Derive** from the manifest's rules. Nothing matched means `Idle`,
//!    never `Blocked`.
//! 4. **Asymmetric hysteresis**: `blocked`/`working` publish at once;
//!    `working -> idle` is held for `IDLE_CONFIRMATIONS` ticks (capped at
//!    `IDLE_HOLD_CAP`) unless a rule gives positive idle evidence.
//! 5. **Edge filter**: only a changed tuple is emitted.
//!
//! Identity distinguishes agent, observed non-agent, and unanswerable
//! (`identify::Occupancy`). Only a confirmed vacancy, seen
//! `VACANT_CONFIRMATIONS` times, retracts; a failed query changes nothing.
//! That is what makes a retraction trustworthy enough to withdraw a human's
//! declaration (`docs/program-sequences.md#the-agent-record`). Occupants are `(pgid, start time)`
//! pairs, so a restart or a recycled pgid is a new occupant.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private server module shared by the sibling terminal_actor / runtime / state modules"
)]

pub(crate) mod identify;
pub mod live_session;
pub use phux_resource::agent_record as record;

mod detector;
mod identity;
mod publish;
mod tick;

// Manifest evaluation lives in `phux-agent-rules`.
pub use phux_agent_rules::DetectedState;
pub(crate) use phux_agent_rules::regions;
pub use phux_agent_rules::rules;

use std::rc::Rc;
use std::time::{Duration, Instant};

use identity::IdentitySource;
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
pub struct AgentReport {
    /// Open-vocabulary kind slug, e.g. `"claude"`.
    pub kind: String,
    /// Human-facing name for the record.
    pub name: String,
    /// The derived lifecycle state.
    pub state: DetectedState,
}

/// Something a pane's actor derived that only `ServerState` can act on,
/// drained by `runtime::client::spawn_agent_state_drain`. Not a wire type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentDetectEvent {
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
    AskSentinel(Option<phux_resource::agent_asked::AskedPayload>),
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
    published_source: phux_resource::agent_state::EvidenceSource,
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
    /// Who answers for the foreground group.
    identity: Box<dyn IdentitySource>,
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
