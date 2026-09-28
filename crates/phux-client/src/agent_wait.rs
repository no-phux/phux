//! Edge-triggered wait on a pane's `phux.agent/v1` lifecycle record
//! (ADR-0076 point 5).
//!
//! Not a level read: `idle` is the detector's fail-safe fallthrough, equally
//! true of a finished agent and a crashed one, so a gate satisfied by the
//! level would pass on a corpse. The wait is satisfied only by an observed
//! transition *into* a target state: [`EdgeTracker::new`] seeds the baseline
//! without evaluating it, and a repeated level is never an edge.
//!
//! `METADATA_CHANGED` pushes are the low-latency half but lossy (dropped on
//! a full mailbox, edge-filtered detector), so a `GET_METADATA` poll floor
//! feeds the same tracker to recover missed edges. The subscription is
//! registered before the baseline read on the same connection, and frames
//! interleaved ahead of the answer are folded in, so no transition slips
//! through the gap.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use phux_protocol::ids::{ResourceId, ResourceKind};
use phux_protocol::wire::frame::{FrameKind, Scope};

use crate::agent_meta::{AgentMetaState, AgentRecord, RESOURCE_AGENT_KEY, parse_agent_record};
use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::state::get_state;
use crate::watch::{FleetSubscription, WatchItem, stream_items, subscribe, subscribe_fleet};

/// The default `--until` set: the three states a turn can end in.
pub const DEFAULT_UNTIL: &[AgentMetaState] = &[
    AgentMetaState::Idle,
    AgentMetaState::Blocked,
    AgentMetaState::Done,
];

/// Correlation id for the pre-wait baseline `GET_METADATA`.
const BASELINE_REQUEST_ID: u32 = 1;

/// Consecutive poll-floor transport failures tolerated once the push half is
/// gone, before the wait gives up.
const POLL_FAILURE_LIMIT: u32 = 3;

/// Parse one `--until` word. `unknown` is not spellable: it is a departure
/// (and the decode of any newer word), not a state to wait for.
#[must_use]
pub fn parse_until(word: &str) -> Option<AgentMetaState> {
    match word {
        "idle" => Some(AgentMetaState::Idle),
        "working" => Some(AgentMetaState::Working),
        "blocked" => Some(AgentMetaState::Blocked),
        "done" => Some(AgentMetaState::Done),
        _ => None,
    }
}

/// How the agent left, when it left rather than settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepartureReason {
    /// The record was deleted or stopped parsing as one.
    Tombstone,
    /// The record's state withdrew to `unknown` (cleared, or a newer word).
    WithdrewToUnknown,
}

impl DepartureReason {
    /// A one-clause explanation for a CLI diagnostic.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tombstone => "the phux.agent/v1 record was deleted",
            Self::WithdrewToUnknown => "the record's state withdrew to unknown",
        }
    }
}

/// What one observation of the record means for a wait in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// No state change, or a change into an untargeted state.
    Pending,
    /// An observed transition into a target state: the only success.
    Satisfied {
        /// The state held before this observation.
        from: AgentMetaState,
        /// The state observed now.
        to: AgentMetaState,
    },
    /// The agent went away mid-wait: neither success nor timeout.
    Departed {
        /// The state held before it went away.
        from: AgentMetaState,
        /// How it went away.
        reason: DepartureReason,
    },
}

/// Which half of the wait learned of an edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeSource {
    /// A server-pushed `METADATA_CHANGED`.
    Push,
    /// The `GET_METADATA` poll floor, recovering a missed edge.
    Poll,
}

impl EdgeSource {
    /// The wire-facing word for this source.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Poll => "poll",
        }
    }
}

/// The transition that satisfied a wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedEdge {
    /// The state held before the transition.
    pub from: AgentMetaState,
    /// The state transitioned into — always a member of the target set.
    pub to: AgentMetaState,
    /// Which half of the wait observed it.
    pub via: EdgeSource,
}

/// Tracks the observation sequence and decides, per observation, whether the
/// wait is satisfied (kept sync so the predicate is unit-testable).
#[derive(Debug)]
pub struct EdgeTracker {
    targets: Vec<AgentMetaState>,
    baseline: AgentMetaState,
    last: AgentMetaState,
    edges: u32,
}

impl EdgeTracker {
    /// Seed a tracker with the pre-wait `baseline` level, which is recorded
    /// and never evaluated (the corpse rule).
    #[must_use]
    pub fn new(baseline: AgentMetaState, targets: &[AgentMetaState]) -> Self {
        Self {
            targets: targets.to_vec(),
            baseline,
            last: baseline,
            edges: 0,
        }
    }

    /// Fold one observation into the wait; `None` (record gone) is a
    /// departure, never a completion.
    pub fn observe(&mut self, record: Option<&AgentRecord>) -> Verdict {
        let Some(record) = record else {
            return Verdict::Departed {
                from: self.last,
                reason: DepartureReason::Tombstone,
            };
        };
        let next = record.state;
        if next == self.last {
            return Verdict::Pending;
        }
        let from = self.last;
        self.last = next;
        self.edges = self.edges.saturating_add(1);
        if next == AgentMetaState::Unknown {
            return Verdict::Departed {
                from,
                reason: DepartureReason::WithdrewToUnknown,
            };
        }
        if self.targets.contains(&next) {
            Verdict::Satisfied { from, to: next }
        } else {
            Verdict::Pending
        }
    }

    /// The pre-wait level this tracker was seeded with.
    #[must_use]
    pub const fn baseline(&self) -> AgentMetaState {
        self.baseline
    }

    /// The most recent state observed.
    #[must_use]
    pub const fn last(&self) -> AgentMetaState {
        self.last
    }

    /// How many state changes have been observed.
    #[must_use]
    pub const fn edges(&self) -> u32 {
        self.edges
    }
}

/// Why a wait could not produce an answer about the agent's lifecycle.
#[derive(Debug, thiserror::Error)]
pub enum AgentWaitError {
    /// The pane declared no record at subscribe time, so it can never
    /// publish an edge; refused up front.
    #[error("pane has no phux.agent/v1 record, so there is no agent lifecycle to wait on")]
    NoRecord,
    /// The agent went away mid-wait.
    #[error("agent departed from '{}' ({})", .from.as_str(), .reason.as_str())]
    Departed {
        /// The state held before the departure.
        from: AgentMetaState,
        /// How it departed.
        reason: DepartureReason,
        /// The last record observed before it went away, for diagnostics.
        last_record: Option<AgentRecord>,
    },
    /// Transport or protocol failure talking to the server.
    #[error(transparent)]
    Transport(#[from] AttachError),
}

/// The outcome of one [`wait_for_agent_state`] call.
#[derive(Debug, Clone)]
pub struct AgentWaitResult {
    /// The transition that satisfied the wait, or `None` if the deadline
    /// elapsed first.
    pub edge: Option<ObservedEdge>,
    /// The level read before the wait began — recorded, never evaluated.
    pub baseline: AgentMetaState,
    /// The most recent state observed.
    pub last: AgentMetaState,
    /// The most recent record observed, carrying the agent's declared
    /// identity.
    pub record: Option<AgentRecord>,
    /// How many state changes were observed in total.
    pub edges: u32,
    /// `GET_METADATA` poll-floor reads performed (excluding the baseline).
    pub polls: u32,
    /// `METADATA_CHANGED` frames received.
    pub pushes: u32,
}

impl AgentWaitResult {
    /// Whether the wait was satisfied by an observed transition.
    #[must_use]
    pub const fn satisfied(&self) -> bool {
        self.edge.is_some()
    }
}

/// The first fleet member observed transitioning into a requested state.
#[derive(Debug, Clone)]
pub struct FleetAgentMatch {
    /// The local Terminal whose agent transitioned.
    pub terminal: ResourceId,
    /// The level held when this Terminal first entered the fleet wait.
    pub baseline: AgentMetaState,
    /// The transition that satisfied the wait.
    pub edge: ObservedEdge,
    /// The agent record observed on the far side of the transition.
    pub record: AgentRecord,
}

/// The outcome of one [`wait_for_any_agent_state`] call.
#[derive(Debug, Clone)]
pub struct FleetAgentWaitResult {
    /// The first matching transition, or `None` when the deadline elapsed.
    pub matched: Option<FleetAgentMatch>,
    /// Agent records still tracked when the wait ended.
    pub agents: usize,
    /// State transitions observed across all tracked agents.
    pub edges: u32,
    /// Poll-floor record reads performed.
    pub polls: u32,
    /// `METADATA_CHANGED` records received.
    pub pushes: u32,
}

impl FleetAgentWaitResult {
    /// Whether an agent transitioned into the requested state.
    #[must_use]
    pub const fn satisfied(&self) -> bool {
        self.matched.is_some()
    }
}

/// Read a pane's `phux.agent/v1` record over `conn`; `None` is "no declared
/// agent", never an error.
async fn read_record(
    conn: &mut Connection,
    terminal: &ResourceId,
    request_id: u32,
) -> Result<(Option<AgentRecord>, Vec<FrameKind>), AttachError> {
    let (answer, interleaved) = conn
        .request_metadata(
            request_id,
            Scope::Resource(terminal.clone()),
            RESOURCE_AGENT_KEY.to_owned(),
        )
        .await?
        .into_parts();
    let value = answer.map_err(|refusal| AttachError::Refused(refusal.to_string()))?;
    Ok((value.as_deref().and_then(parse_agent_record), interleaved))
}

/// Read a pane's `phux.agent/v1` record on a fresh, side-effect-free
/// connection (the poll floor's read).
///
/// # Errors
///
/// Propagates [`AttachError`] from connect, transport, or a server refusal.
pub async fn fetch_agent_record(
    socket: &Path,
    terminal: &ResourceId,
) -> Result<Option<AgentRecord>, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    // Nothing is subscribed here, so nothing interleaved matters.
    let (record, _interleaved) = read_record(&mut conn, terminal, BASELINE_REQUEST_ID).await?;
    Ok(record)
}

/// The record an interleaved `METADATA_CHANGED` carries for `terminal`
/// (`Some(None)` is a tombstone), or `None` if the frame is not ours.
#[allow(
    clippy::option_option,
    reason = "outer: is this frame ours; inner: record or tombstone"
)]
pub(crate) fn record_from_frame(
    frame: &FrameKind,
    terminal: &ResourceId,
) -> Option<Option<AgentRecord>> {
    let FrameKind::MetadataChanged {
        scope, key, value, ..
    } = frame
    else {
        return None;
    };
    if key != RESOURCE_AGENT_KEY {
        return None;
    }
    let Scope::Resource(id) = scope else {
        return None;
    };
    if id != terminal {
        return None;
    }
    Some(value.as_deref().and_then(parse_agent_record))
}

/// Counters the two halves of the wait share.
#[derive(Debug, Default, Clone, Copy)]
struct Stats {
    polls: u32,
    pushes: u32,
}

/// One decided observation: the verdict, the record it came from, and which
/// half of the wait saw it.
pub(crate) type Decision = (Verdict, Option<AgentRecord>, EdgeSource);

/// State the two halves of the wait share; both are polled by one task, so
/// the borrows never overlap.
pub(crate) struct WaitShared {
    tracker: RefCell<EdgeTracker>,
    latest: RefCell<Option<AgentRecord>>,
    stats: RefCell<Stats>,
    pub(crate) push_ended: Cell<bool>,
}

impl WaitShared {
    /// Carry the replayed tracker and last-seen record into the live wait.
    pub(crate) fn new(tracker: EdgeTracker, latest: Option<AgentRecord>) -> Self {
        Self {
            tracker: RefCell::new(tracker),
            latest: RefCell::new(latest),
            stats: RefCell::new(Stats::default()),
            push_ended: Cell::new(false),
        }
    }

    /// Count one observation from `via`, fold it into the tracker, and
    /// remember the record unless it was a tombstone.
    fn observe(&self, record: Option<&AgentRecord>, via: EdgeSource) -> Verdict {
        {
            let mut stats = self.stats.borrow_mut();
            match via {
                EdgeSource::Push => stats.pushes = stats.pushes.saturating_add(1),
                EdgeSource::Poll => stats.polls = stats.polls.saturating_add(1),
            }
        }
        let verdict = self.tracker.borrow_mut().observe(record);
        if let Some(record) = record {
            *self.latest.borrow_mut() = Some(record.clone());
        }
        verdict
    }

    /// Take the state apart once both halves are done with it.
    fn into_parts(self) -> (EdgeTracker, Option<AgentRecord>, Stats) {
        (
            self.tracker.into_inner(),
            self.latest.into_inner(),
            self.stats.into_inner(),
        )
    }
}

/// What replaying the subscribe/read window left the wait in.
enum Replay {
    /// The window already decided the wait; neither live half ever runs.
    Decided(AgentWaitResult),
    /// Nothing decided: carry this state into the live wait.
    Watching(WaitShared),
}

/// The subscribe/read window's observations in time order: what the server
/// published before the answer, then the answer (so a flicker inside the
/// window stays visible).
fn window_observations(
    interleaved: &[FrameKind],
    answered: Option<AgentRecord>,
    terminal: &ResourceId,
) -> Vec<Option<AgentRecord>> {
    let mut observations: Vec<Option<AgentRecord>> = interleaved
        .iter()
        .filter_map(|frame| record_from_frame(frame, terminal))
        .collect();
    observations.push(answered);
    observations
}

/// Seed the tracker from the baseline observation, then replay everything
/// that arrived inside the subscribe/read window through it.
fn replay_window(
    observations: Vec<Option<AgentRecord>>,
    targets: &[AgentMetaState],
) -> Result<Replay, AgentWaitError> {
    let mut sequence = observations.into_iter();
    // `window_observations` pushes the answer, so there is at least one.
    let baseline_record = sequence.next().flatten();
    let Some(baseline_record) = baseline_record else {
        return Err(AgentWaitError::NoRecord);
    };
    let mut tracker = EdgeTracker::new(baseline_record.state, targets);
    let mut latest = Some(baseline_record);

    for observation in sequence {
        let verdict = tracker.observe(observation.as_ref());
        if observation.is_some() {
            latest.clone_from(&observation);
        }
        match verdict {
            Verdict::Pending => {}
            Verdict::Satisfied { from, to } => {
                return Ok(Replay::Decided(AgentWaitResult {
                    edge: Some(ObservedEdge {
                        from,
                        to,
                        via: EdgeSource::Push,
                    }),
                    baseline: tracker.baseline(),
                    last: tracker.last(),
                    record: latest,
                    edges: tracker.edges(),
                    polls: 0,
                    pushes: 0,
                }));
            }
            Verdict::Departed { from, reason } => {
                return Err(AgentWaitError::Departed {
                    from,
                    reason,
                    last_record: latest,
                });
            }
        }
    }
    Ok(Replay::Watching(WaitShared::new(tracker, latest)))
}

/// The low-latency half: fold `METADATA_CHANGED` pushes in until one decides
/// the wait, then park forever if the stream ends without deciding.
#[allow(
    clippy::future_not_send,
    reason = "halves share a RefCell on one task (ADR-0003)"
)]
pub(crate) async fn watch_pushes(
    conn: &mut Connection,
    shared: &WaitShared,
) -> Result<Decision, AgentWaitError> {
    let mut decided: Option<Decision> = None;
    let streamed = stream_items(conn, |item| {
        let WatchItem::AgentState(update) = item else {
            return true;
        };
        let verdict = shared.observe(update.record.as_ref(), EdgeSource::Push);
        if matches!(verdict, Verdict::Pending) {
            return true;
        }
        decided = Some((verdict, update.record, EdgeSource::Push));
        false
    })
    .await;
    shared.push_ended.set(true);
    // An undecided end is not fatal: the poll floor keeps serving the wait.
    let _ = streamed;
    match decided {
        Some(decision) => Ok(decision),
        None => std::future::pending::<Result<Decision, AgentWaitError>>().await,
    }
}

/// The recovery half: re-read the record on the `wait` cadence and decide
/// from any level that differs from the one last held. With `fail_after`,
/// gives up after that many consecutive failures once the push half ended.
#[allow(
    clippy::future_not_send,
    reason = "halves share a RefCell on one task (ADR-0003)"
)]
pub(crate) async fn poll_floor(
    socket: &Path,
    terminal: &ResourceId,
    poll_interval: Duration,
    shared: &WaitShared,
    fail_after: Option<u32>,
) -> Result<Decision, AgentWaitError> {
    let mut failures: u32 = 0;
    loop {
        tokio::time::sleep(poll_interval).await;
        match fetch_agent_record(socket, terminal).await {
            Ok(record) => {
                failures = 0;
                let verdict = shared.observe(record.as_ref(), EdgeSource::Poll);
                if !matches!(verdict, Verdict::Pending) {
                    return Ok((verdict, record, EdgeSource::Poll));
                }
            }
            Err(err) => {
                failures = failures.saturating_add(1);
                if fail_after.is_some_and(|limit| failures >= limit) && shared.push_ended.get() {
                    return Err(AgentWaitError::Transport(err));
                }
            }
        }
    }
}

/// Elapse after `timeout`, or never when the caller set none.
pub(crate) async fn deadline(timeout: Option<Duration>) {
    match timeout {
        Some(limit) => tokio::time::sleep(limit).await,
        None => std::future::pending::<()>().await,
    }
}

/// Fold the decided verdict — or the deadline's absence of one — into the
/// call's result.
pub(crate) fn finish(
    decision: Option<Decision>,
    shared: WaitShared,
) -> Result<AgentWaitResult, AgentWaitError> {
    let (tracker, latest, stats) = shared.into_parts();
    // `latest` already holds a satisfying record; a departure's may be a
    // tombstone, so it falls back to the last record seen.
    let edge = match decision {
        Some((Verdict::Departed { from, reason }, record, _)) => {
            return Err(AgentWaitError::Departed {
                from,
                reason,
                last_record: record.or(latest),
            });
        }
        Some((Verdict::Satisfied { from, to }, _, via)) => Some(ObservedEdge { from, to, via }),
        // The halves never return `Pending`; it folds in with the deadline.
        Some((Verdict::Pending, _, _)) | None => None,
    };
    Ok(AgentWaitResult {
        edge,
        baseline: tracker.baseline(),
        last: tracker.last(),
        record: latest,
        edges: tracker.edges(),
        polls: stats.polls,
        pushes: stats.pushes,
    })
}

/// Wait until `terminal`'s agent record transitions into one of `targets`,
/// or `timeout` elapses.
///
/// Over the merged push/poll observations `O_0` (the baseline), `O_1`, …,
/// the wait is satisfied at the first `i > 0` whose state differs from the
/// last held and is a target; it departs at a tombstone or a change to
/// `unknown`. A pane resting at `idle` never satisfies `--until idle`.
///
/// # Errors
///
/// [`AgentWaitError::NoRecord`] when the pane declares no record at subscribe
/// time; [`AgentWaitError::Departed`] when the agent goes away mid-wait;
/// [`AgentWaitError::Transport`] on connect/transport failure.
#[allow(
    clippy::future_not_send,
    reason = "halves share a RefCell on one task (ADR-0003)"
)]
pub async fn wait_for_agent_state(
    socket: &Path,
    terminal: &ResourceId,
    targets: &[AgentMetaState],
    timeout: Option<Duration>,
    poll_interval: Duration,
) -> Result<AgentWaitResult, AgentWaitError> {
    // Subscribe first, then read the baseline on the same connection.
    let mut conn = subscribe(socket, Some(terminal.clone())).await?;
    let (answered, interleaved) = read_record(&mut conn, terminal, BASELINE_REQUEST_ID).await?;

    let observations = window_observations(&interleaved, answered, terminal);
    let shared = match replay_window(observations, targets)? {
        Replay::Decided(result) => return Ok(result),
        Replay::Watching(shared) => shared,
    };

    let decision: Option<Decision> = tokio::select! {
        decided = watch_pushes(&mut conn, &shared) => Some(decided?),
        decided = poll_floor(socket, terminal, poll_interval, &shared, Some(POLL_FAILURE_LIMIT)) => Some(decided?),
        () = deadline(timeout) => None,
    };
    drop(conn);

    finish(decision, shared)
}

#[derive(Debug, Default)]
struct FleetStats {
    edges: u32,
    polls: u32,
    pushes: u32,
}

#[derive(Debug)]
struct FleetTrackers<'a> {
    targets: &'a [AgentMetaState],
    agents: HashMap<ResourceId, EdgeTracker>,
    stats: FleetStats,
}

impl<'a> FleetTrackers<'a> {
    fn new(targets: &'a [AgentMetaState]) -> Self {
        Self {
            targets,
            agents: HashMap::new(),
            stats: FleetStats::default(),
        }
    }

    fn remove(&mut self, terminal: &ResourceId) {
        self.agents.remove(terminal);
    }

    fn observe(
        &mut self,
        terminal: ResourceId,
        record: Option<AgentRecord>,
        via: Option<EdgeSource>,
    ) -> Option<FleetAgentMatch> {
        match via {
            Some(EdgeSource::Push) => self.stats.pushes = self.stats.pushes.saturating_add(1),
            Some(EdgeSource::Poll) => self.stats.polls = self.stats.polls.saturating_add(1),
            None => {}
        }
        let Some(record) = record else {
            self.agents.remove(&terminal);
            return None;
        };
        let Some(tracked) = self.agents.get_mut(&terminal) else {
            self.agents
                .insert(terminal, EdgeTracker::new(record.state, self.targets));
            return None;
        };
        let before = tracked.edges();
        let verdict = tracked.observe(Some(&record));
        self.stats.edges = self
            .stats
            .edges
            .saturating_add(tracked.edges().saturating_sub(before));
        match verdict {
            Verdict::Satisfied { from, to } => Some(FleetAgentMatch {
                terminal,
                baseline: tracked.baseline(),
                edge: ObservedEdge {
                    from,
                    to,
                    via: via.unwrap_or(EdgeSource::Push),
                },
                record,
            }),
            Verdict::Departed { .. } => {
                self.agents.remove(&terminal);
                None
            }
            Verdict::Pending => None,
        }
    }

    fn result(self, matched: Option<FleetAgentMatch>) -> FleetAgentWaitResult {
        FleetAgentWaitResult {
            matched,
            agents: self.agents.len(),
            edges: self.stats.edges,
            polls: self.stats.polls,
            pushes: self.stats.pushes,
        }
    }
}

const fn next_request_id(request_id: &mut u32) -> u32 {
    let current = *request_id;
    *request_id = request_id.wrapping_add(1);
    if *request_id == 0 {
        *request_id = 1;
    }
    current
}

async fn fold_fleet_frame(
    frame: FrameKind,
    subscription: &mut FleetSubscription,
    trackers: &mut FleetTrackers<'_>,
    needs_baseline: &mut Vec<ResourceId>,
) -> Result<Option<FleetAgentMatch>, AttachError> {
    match frame {
        FrameKind::MetadataChanged {
            scope, key, value, ..
        } if key == RESOURCE_AGENT_KEY => {
            let Scope::Resource(terminal) = scope else {
                return Ok(None);
            };
            if !subscription.terminals.contains(&terminal) {
                return Ok(None);
            }
            Ok(trackers.observe(
                terminal,
                value.as_deref().and_then(parse_agent_record),
                Some(EdgeSource::Push),
            ))
        }
        FrameKind::Event {
            terminal: Some(terminal),
            event:
                phux_protocol::wire::frame::AgentEvent::ResourceSpawned {
                    kind: ResourceKind::Terminal,
                    ..
                },
            ..
        } => {
            if subscription.subscribe_terminal(terminal.clone()).await? {
                needs_baseline.push(terminal);
            }
            Ok(None)
        }
        FrameKind::Event {
            terminal: Some(terminal),
            event: phux_protocol::wire::frame::AgentEvent::ResourceClosed { .. },
            ..
        } => {
            subscription.remove_terminal(&terminal);
            trackers.remove(&terminal);
            Ok(None)
        }
        _ => Ok(None),
    }
}

async fn baseline_fleet_terminals(
    subscription: &mut FleetSubscription,
    trackers: &mut FleetTrackers<'_>,
    terminals: Vec<ResourceId>,
    request_id: &mut u32,
) -> Result<Option<FleetAgentMatch>, AttachError> {
    let mut pending = terminals;
    while let Some(terminal) = pending.pop() {
        if !subscription.terminals.contains(&terminal) {
            continue;
        }
        let (record, interleaved) = read_record(
            &mut subscription.conn,
            &terminal,
            next_request_id(request_id),
        )
        .await?;
        for frame in interleaved {
            if let Some(matched) =
                fold_fleet_frame(frame, subscription, trackers, &mut pending).await?
            {
                return Ok(Some(matched));
            }
        }
        if subscription.terminals.contains(&terminal)
            && let Some(matched) = trackers.observe(terminal, record, None)
        {
            return Ok(Some(matched));
        }
    }
    Ok(None)
}

fn local_terminals(view: &crate::state::StateView) -> HashSet<ResourceId> {
    view.snapshot()
        .resources
        .iter()
        .filter(|resource| resource.kind == ResourceKind::Terminal && resource.id.is_local())
        .map(|resource| resource.id.clone())
        .collect()
}

/// Wait until any local agent in the fleet transitions into one of
/// `targets`, or `timeout` elapses.
///
/// Panes are enumerated after the lifecycle stream is subscribed; each local
/// Terminal gets its own L3 subscription and tracker, and a periodic sweep
/// is the convergence floor for dropped notifications and stale panes.
/// Initial levels never satisfy the wait. Satellites are excluded: L3
/// metadata does not federate.
///
/// # Errors
///
/// Returns [`AgentWaitError::Transport`] when initial setup fails, or when the
/// push stream has ended and three consecutive poll sweeps cannot reach the
/// server.
#[expect(
    clippy::significant_drop_tightening,
    reason = "fleet push subscription remains live across every select-loop iteration"
)]
pub async fn wait_for_any_agent_state(
    socket: &Path,
    targets: &[AgentMetaState],
    timeout: Option<Duration>,
    poll_interval: Duration,
) -> Result<FleetAgentWaitResult, AgentWaitError> {
    let mut subscription = subscribe_fleet(socket).await?;
    // `subscribe_fleet` already folded these lifecycle events into its set.
    // No metadata could precede the subscriptions it installs afterwards.
    let _ = subscription.take_pending();
    let mut trackers = FleetTrackers::new(targets);
    let mut request_id = 1;
    let mut initial: Vec<ResourceId> = subscription.terminals.iter().cloned().collect();
    initial.sort_by(|left, right| right.cmp(left));
    if let Some(matched) =
        baseline_fleet_terminals(&mut subscription, &mut trackers, initial, &mut request_id).await?
    {
        return Ok(trackers.result(Some(matched)));
    }

    let mut push_ended = false;
    let mut poll_failures = 0_u32;
    let mut interval =
        tokio::time::interval_at(tokio::time::Instant::now() + poll_interval, poll_interval);
    let deadline = deadline(timeout);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            frame = subscription.conn.recv(), if !push_ended => {
                match frame {
                    Ok(frame) => {
                        let mut baselines = Vec::new();
                        if let Some(matched) =
                            fold_fleet_frame(frame, &mut subscription, &mut trackers, &mut baselines).await?
                        {
                            return Ok(trackers.result(Some(matched)));
                        }
                        if let Some(matched) = baseline_fleet_terminals(
                            &mut subscription,
                            &mut trackers,
                            baselines,
                            &mut request_id,
                        ).await? {
                            return Ok(trackers.result(Some(matched)));
                        }
                    }
                    Err(_err) => push_ended = true,
                }
            }
            _ = interval.tick() => {
                let view = match get_state(socket).await {
                    Ok(view) => view,
                    Err(err) => {
                        poll_failures = poll_failures.saturating_add(1);
                        if push_ended && poll_failures >= POLL_FAILURE_LIMIT {
                            return Err(AgentWaitError::Transport(err));
                        }
                        continue;
                    }
                };
                poll_failures = 0;
                let current = local_terminals(&view);
                let stale: Vec<ResourceId> = subscription
                    .terminals
                    .difference(&current)
                    .cloned()
                    .collect();
                for terminal in stale {
                    subscription.remove_terminal(&terminal);
                    trackers.remove(&terminal);
                }
                for terminal in &current {
                    if !push_ended && !subscription.terminals.contains(terminal)
                        && subscription.subscribe_terminal(terminal.clone()).await.is_err()
                    {
                        push_ended = true;
                    }
                    match fetch_agent_record(socket, terminal).await {
                        Ok(record) => {
                            poll_failures = 0;
                            if let Some(matched) = trackers.observe(
                                terminal.clone(),
                                record,
                                Some(EdgeSource::Poll),
                            ) {
                                return Ok(trackers.result(Some(matched)));
                            }
                        }
                        Err(err) => {
                            poll_failures = poll_failures.saturating_add(1);
                            if push_ended && poll_failures >= POLL_FAILURE_LIMIT {
                                return Err(AgentWaitError::Transport(err));
                            }
                        }
                    }
                }
                // Poll-only members still count for pruning and `agents`.
                subscription.terminals.extend(current);
            }
            () = &mut deadline => return Ok(trackers.result(None)),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests"
    )]
    #![allow(clippy::future_not_send, reason = "the wait is !Send by design")]

    use tokio::net::UnixListener;

    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

    use crate::testkit::{EndOfScript, ScriptSpec, ScriptedServer, serve_every};

    use super::*;

    fn record(state: &str) -> Vec<u8> {
        format!(r#"{{"name":"reviewer","kind":"claude","state":"{state}"}}"#).into_bytes()
    }

    fn changed(pane: &ResourceId, value: Option<Vec<u8>>) -> FrameKind {
        FrameKind::MetadataChanged {
            scope: Scope::Resource(pane.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value,
            actor: None,
        }
    }

    fn state_record(state: AgentMetaState) -> AgentRecord {
        AgentRecord {
            name: "reviewer".to_owned(),
            state,
            ..AgentRecord::default()
        }
    }

    fn fleet_snapshot(terminals: &[ResourceId]) -> SessionSnapshot {
        SessionSnapshot::new(
            SessionId::new(1),
            WindowId::new(1),
            terminals.first().cloned().unwrap_or_default(),
        )
        .with_resources(
            terminals
                .iter()
                .cloned()
                .map(|terminal| ResourceInfo::new(terminal, WindowId::new(1), 80, 24))
                .collect(),
        )
    }

    /// Drive `wait_for_agent_state` against the scripted server: `level`
    /// answers every `GET_METADATA`; `pushes` go to the first connection.
    /// Each connection gets its own task, since the subscribed one lives as
    /// long as the wait while the poll floor dials fresh ones.
    async fn drive(
        level: Option<Vec<u8>>,
        pushes: Vec<FrameKind>,
        targets: &[AgentMetaState],
        timeout: Duration,
    ) -> Result<AgentWaitResult, AgentWaitError> {
        let pane = ResourceId::local(7);
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        let server = tokio::spawn(async move {
            let mut first = true;
            loop {
                let (stream, _) = listener.accept().await.expect("accept scripted client");
                let level = level.clone();
                let mut spec = ScriptSpec::new().metadata(move |_scope, key| {
                    if key == RESOURCE_AGENT_KEY {
                        level.clone()
                    } else {
                        None
                    }
                });
                if first {
                    spec = spec.extend(pushes.clone());
                    first = false;
                }
                // Stay up: the corpse test needs a live server answering idle.
                let spec = spec.end(EndOfScript::ServeUntilDetach);
                tokio::spawn(async move {
                    ScriptedServer::on_stream(stream, spec).run().await;
                });
            }
        });
        let outcome = wait_for_agent_state(
            &socket,
            &pane,
            targets,
            Some(timeout),
            Duration::from_millis(30),
        )
        .await;
        server.abort();
        outcome
    }

    /// A pane resting at `idle` must not satisfy `--until idle`, however
    /// often the level is re-read.
    #[test]
    fn a_level_read_of_idle_never_satisfies_the_wait() {
        let mut tracker = EdgeTracker::new(AgentMetaState::Idle, &[AgentMetaState::Idle]);
        for _ in 0..100 {
            assert_eq!(
                tracker.observe(Some(&state_record(AgentMetaState::Idle))),
                Verdict::Pending,
                "a repeated level read must never satisfy a completion gate"
            );
        }
        assert_eq!(tracker.edges(), 0);
    }

    #[test]
    fn a_working_to_idle_transition_satisfies_the_wait() {
        let mut tracker = EdgeTracker::new(AgentMetaState::Working, &[AgentMetaState::Idle]);
        assert_eq!(
            tracker.observe(Some(&state_record(AgentMetaState::Idle))),
            Verdict::Satisfied {
                from: AgentMetaState::Working,
                to: AgentMetaState::Idle,
            }
        );
        assert_eq!(tracker.edges(), 1);
    }

    /// An already-blocked pane must still transition to satisfy `blocked`.
    #[test]
    fn an_already_blocked_baseline_needs_a_transition_too() {
        let mut tracker = EdgeTracker::new(AgentMetaState::Blocked, &[AgentMetaState::Blocked]);
        assert_eq!(
            tracker.observe(Some(&state_record(AgentMetaState::Blocked))),
            Verdict::Pending
        );
        assert_eq!(
            tracker.observe(Some(&state_record(AgentMetaState::Working))),
            Verdict::Pending
        );
        assert_eq!(
            tracker.observe(Some(&state_record(AgentMetaState::Blocked))),
            Verdict::Satisfied {
                from: AgentMetaState::Working,
                to: AgentMetaState::Blocked,
            }
        );
    }

    /// A transition into a state nobody asked for keeps the wait open, and
    /// re-arms the edge: the next transition is measured from there.
    #[test]
    fn an_untargeted_transition_advances_the_baseline_without_satisfying() {
        let mut tracker = EdgeTracker::new(AgentMetaState::Idle, &[AgentMetaState::Done]);
        assert_eq!(
            tracker.observe(Some(&state_record(AgentMetaState::Working))),
            Verdict::Pending
        );
        assert_eq!(tracker.last(), AgentMetaState::Working);
        assert_eq!(
            tracker.observe(Some(&state_record(AgentMetaState::Done))),
            Verdict::Satisfied {
                from: AgentMetaState::Working,
                to: AgentMetaState::Done,
            }
        );
    }

    /// A tombstone or a withdrawal to `unknown` is a departure, never a
    /// completion.
    #[test]
    fn a_tombstone_or_withdrawal_is_a_departure_not_a_completion() {
        for (observed, reason) in [
            (None, DepartureReason::Tombstone),
            (
                Some(state_record(AgentMetaState::Unknown)),
                DepartureReason::WithdrewToUnknown,
            ),
        ] {
            let mut tracker = EdgeTracker::new(AgentMetaState::Working, DEFAULT_UNTIL);
            assert_eq!(
                tracker.observe(observed.as_ref()),
                Verdict::Departed {
                    from: AgentMetaState::Working,
                    reason,
                }
            );
        }
    }

    #[test]
    fn until_vocabulary_excludes_unknown_and_newer_words() {
        assert_eq!(parse_until("working"), Some(AgentMetaState::Working));
        for word in ["unknown", "hibernating", ""] {
            assert_eq!(parse_until(word), None, "{word}");
        }
    }

    /// An interleaved `METADATA_CHANGED` for another key, another scope, or
    /// another Terminal is not one of ours.
    #[test]
    fn interleaved_frames_are_filtered_to_this_terminals_agent_key() {
        let pane = ResourceId::local(7);
        let other = ResourceId::local(8);
        assert!(record_from_frame(&changed(&pane, Some(record("idle"))), &pane).is_some());
        assert!(record_from_frame(&changed(&other, Some(record("idle"))), &pane).is_none());
        assert!(
            record_from_frame(
                &FrameKind::MetadataChanged {
                    scope: Scope::Resource(pane.clone()),
                    key: "phux.tags/v1".to_owned(),
                    value: Some(record("idle")),
                    actor: None,
                },
                &pane,
            )
            .is_none()
        );
        // A tombstone for our key IS one of ours, carrying `None`.
        assert_eq!(record_from_frame(&changed(&pane, None), &pane), Some(None));
    }

    /// The corpse case end to end: a pane answering `idle` on every read and
    /// never changing times out rather than succeeding.
    #[tokio::test]
    async fn a_live_pane_resting_at_idle_times_out_rather_than_succeeding() {
        let outcome = drive(
            Some(record("idle")),
            Vec::new(),
            &[AgentMetaState::Idle],
            Duration::from_millis(250),
        )
        .await
        .expect("a resting pane is a timeout, not an error");
        assert!(
            !outcome.satisfied(),
            "a level read of idle must never satisfy a completion gate: {outcome:?}"
        );
        assert_eq!(outcome.baseline, AgentMetaState::Idle);
        assert_eq!(outcome.last, AgentMetaState::Idle);
        assert_eq!(outcome.edges, 0);
    }

    /// An observed `working -> blocked` transition satisfies the wait, naming
    /// the edge, its source, and the record's identity.
    #[tokio::test]
    async fn an_observed_transition_satisfies_the_wait_end_to_end() {
        let pane = ResourceId::local(7);
        let outcome = drive(
            Some(record("blocked")),
            vec![
                changed(&pane, Some(record("working"))),
                changed(&pane, Some(record("blocked"))),
            ],
            &[AgentMetaState::Blocked],
            Duration::from_millis(2_000),
        )
        .await
        .expect("an observed transition is a success");

        assert!(outcome.satisfied(), "{outcome:?}");
        let edge = outcome.edge.expect("a satisfied wait carries its edge");
        assert_eq!(edge.from, AgentMetaState::Working);
        assert_eq!(edge.to, AgentMetaState::Blocked);
        assert_eq!(edge.via, EdgeSource::Push);
        assert_eq!(
            outcome
                .record
                .expect("a satisfied wait carries a record")
                .name,
            "reviewer"
        );
    }

    /// A record that goes away mid-wait ends it with a typed departure.
    #[tokio::test]
    async fn a_tombstone_mid_wait_is_a_typed_departure_end_to_end() {
        let pane = ResourceId::local(7);
        let outcome = drive(
            Some(record("working")),
            vec![
                changed(&pane, Some(record("working"))),
                changed(&pane, None),
            ],
            DEFAULT_UNTIL,
            Duration::from_millis(2_000),
        )
        .await;
        assert!(
            matches!(
                outcome,
                Err(AgentWaitError::Departed {
                    reason: DepartureReason::Tombstone,
                    ..
                })
            ),
            "got {outcome:?}"
        );
    }

    /// A pane with no record is refused up front.
    #[tokio::test]
    async fn an_absent_record_is_refused_immediately() {
        let outcome = drive(
            None,
            Vec::new(),
            DEFAULT_UNTIL,
            Duration::from_millis(2_000),
        )
        .await;
        assert!(
            matches!(outcome, Err(AgentWaitError::NoRecord)),
            "got {outcome:?}"
        );
    }

    #[test]
    fn fleet_tracking_is_per_agent_filters_states_and_ignores_stale_levels() {
        let first = ResourceId::local(7);
        let second = ResourceId::local(8);
        let mut fleet = FleetTrackers::new(&[AgentMetaState::Blocked]);
        let mut see = |terminal: &ResourceId, state, via| {
            fleet.observe(terminal.clone(), Some(state_record(state)), via)
        };
        // An already-blocked baseline is a stale level; an untargeted
        // transition advances only that agent's tracker.
        assert!(see(&first, AgentMetaState::Blocked, None).is_none());
        assert!(see(&second, AgentMetaState::Working, None).is_none());
        assert!(see(&second, AgentMetaState::Idle, Some(EdgeSource::Push)).is_none());
        let matched = see(&second, AgentMetaState::Blocked, Some(EdgeSource::Push))
            .expect("the second agent's transition matches");
        assert_eq!(matched.terminal, second);
        assert_eq!(matched.edge.from, AgentMetaState::Idle);
        assert_eq!(matched.edge.to, AgentMetaState::Blocked);
        assert_eq!(fleet.agents.len(), 2);

        assert!(
            fleet
                .observe(first.clone(), None, Some(EdgeSource::Push))
                .is_none()
        );
        assert!(
            fleet.agents.contains_key(&second),
            "one departing agent must not terminate or erase its peers"
        );
        assert!(!fleet.agents.contains_key(&first));
    }

    #[tokio::test]
    async fn fleet_wait_returns_the_agent_that_transitions_not_the_first_baseline() {
        let first = ResourceId::local(7);
        let second = ResourceId::local(8);
        let snapshot = fleet_snapshot(&[first.clone(), second.clone()]);
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        let pushed_second = second.clone();
        let spec = ScriptSpec::new()
            .state(snapshot)
            .metadata(|_scope, key| (key == RESOURCE_AGENT_KEY).then(|| record("working")))
            .push_after_subscribe(
                Scope::Resource(second.clone()),
                RESOURCE_AGENT_KEY,
                vec![
                    changed(&pushed_second, Some(record("working"))),
                    changed(&pushed_second, Some(record("blocked"))),
                ],
            )
            .end(EndOfScript::ServeUntilDetach);
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let result = wait_for_any_agent_state(
            &socket,
            &[AgentMetaState::Blocked],
            Some(Duration::from_secs(2)),
            Duration::from_millis(30),
        )
        .await
        .expect("fleet transition");
        let matched = result.matched.expect("wait was satisfied");
        assert_eq!(matched.terminal, second);
        assert_eq!(matched.edge.from, AgentMetaState::Working);
        assert_eq!(matched.edge.to, AgentMetaState::Blocked);
        assert_eq!(matched.edge.via, EdgeSource::Push);
        server.await.expect("scripted server task");
    }

    #[tokio::test]
    async fn fleet_wait_times_out_when_multiple_agents_hold_stale_target_levels() {
        let terminals = vec![ResourceId::local(7), ResourceId::local(8)];
        let snapshot = fleet_snapshot(&terminals);
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        let server = tokio::spawn(async move {
            serve_every(listener, move || {
                ScriptSpec::new()
                    .state(snapshot.clone())
                    .metadata(|_scope, key| (key == RESOURCE_AGENT_KEY).then(|| record("blocked")))
                    .end(EndOfScript::ServeUntilDetach)
            })
            .await;
        });

        let result = wait_for_any_agent_state(
            &socket,
            &[AgentMetaState::Blocked],
            Some(Duration::from_millis(180)),
            Duration::from_millis(30),
        )
        .await
        .expect("stale levels time out cleanly");
        assert!(!result.satisfied());
        assert_eq!(result.agents, 2);
        assert_eq!(result.edges, 0);
        assert!(result.polls >= 2, "the convergence floor ran: {result:?}");
        server.abort();
    }

    #[tokio::test]
    async fn fleet_wait_reports_disconnect_during_initial_enumeration() {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        let server = tokio::spawn(async move {
            ScriptedServer::accept(
                &listener,
                ScriptSpec::new()
                    .push(FrameKind::Event {
                        terminal: None,
                        event: phux_protocol::wire::frame::AgentEvent::Bell,
                        stamp: None,
                    })
                    .end(EndOfScript::HangUp),
            )
            .await
        });
        let outcome = wait_for_any_agent_state(
            &socket,
            DEFAULT_UNTIL,
            Some(Duration::from_secs(1)),
            Duration::from_millis(20),
        )
        .await;
        assert!(
            matches!(
                outcome,
                Err(AgentWaitError::Transport(AttachError::Disconnected))
            ),
            "got {outcome:?}"
        );
        server.await.expect("scripted server task");
    }
}
