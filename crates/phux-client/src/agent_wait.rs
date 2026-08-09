//! Edge-triggered wait on a pane's `phux.agent/v1` lifecycle record
//! (ADR-0076 point 5).
//!
//! # Why this is not a level read
//!
//! The obvious implementation — `GET_METADATA`, and if the state is already
//! in the caller's target set exit 0 — is wrong, and wrong in the direction
//! that hurts. `idle` is the detector's **fail-safe fallthrough**
//! (`crates/phux-server/src/agent_detect/mod.rs`): the five shipped
//! manifests carry five `state = "working"` rules and five
//! `state = "blocked"` rules and *no positive `idle` rule at all* —
//! `claude.toml` documents that its authors deliberately declined to write
//! one. So `idle` asserts only "no state-bearing rule matched", which is
//! equally true of a finished agent, a half-painted TUI, a **crashed** agent,
//! a pane running `less`, and every pane on a machine with no manifest
//! loaded. A completion gate satisfied by that level returns success on a
//! corpse, instantly.
//!
//! This module therefore reads `idle` two different ways, per the governing
//! ruling:
//!
//! - as a **level** it is absence of contrary evidence — the right predicate
//!   for a *safety* gate ("do not disturb this pane"), which is not what this
//!   module is;
//! - as the far side of a **`working -> idle` edge** it is positive evidence
//!   that whatever was asserting `working` stopped asserting it — which is
//!   what a *completion* gate needs.
//!
//! [`wait_for_agent_state`] is satisfied only by the second. The pre-wait
//! `GET_METADATA` exists to establish the baseline the edge is measured
//! against; it can never itself satisfy the wait. That is enforced
//! structurally: [`EdgeTracker::new`] *seeds* the baseline and never
//! evaluates it against the target set, and [`EdgeTracker::observe`] returns
//! [`Verdict::Pending`] for any observation whose state equals the last one
//! held.
//!
//! # Why both a subscription and a poll floor
//!
//! `SUBSCRIBE_METADATA` (0x54) + `METADATA_CHANGED` (0xD0) is the
//! low-latency half, and it is lossy in two documented ways: the server
//! delivers the notification with `try_send` and drops it on a full mailbox
//! (`state/client_table.rs`, "a dropped notification is acceptable"), and the
//! detector is edge-filtered, publishing only on a changed `(kind, name,
//! state)` tuple. So the CLI also re-reads `GET_METADATA` on the ordinary
//! `wait` cadence and treats a value differing from the one it last held as
//! the edge it missed. That is level-triggered **recovery of an edge**, not a
//! level gate: the poll's answer is fed through the same [`EdgeTracker`] and
//! is subject to the same "must differ from the last state held" rule.
//!
//! The subscription is registered **before** the baseline is read, on the
//! same connection ([`crate::watch::subscribe`]), so no transition can slip
//! through the gap between the two: the server handles one connection's
//! frames in order, and anything it published in between arrives ahead of the
//! `METADATA_VALUE` as an interleaved frame, which this module folds into the
//! observation sequence rather than dropping.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::time::Duration;

use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{FrameKind, Scope};

use crate::agent_meta::{AgentMetaState, AgentRecord, TERMINAL_AGENT_KEY, parse_agent_record};
use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::watch::{WatchItem, stream_items, subscribe};

/// The default `--until` set: the three states a turn can end in
/// (ADR-0076 point 5). `working` is spellable but not a default — it is the
/// start of a turn, not the end of one.
pub const DEFAULT_UNTIL: &[AgentMetaState] = &[
    AgentMetaState::Idle,
    AgentMetaState::Blocked,
    AgentMetaState::Done,
];

/// Correlation id for the pre-wait baseline `GET_METADATA`.
const BASELINE_REQUEST_ID: u32 = 1;

/// Consecutive poll-floor transport failures tolerated once the push half is
/// already gone, before the wait gives up with a transport error. A single
/// hiccup on a busy socket must not fail a wait the subscription is still
/// serving.
const POLL_FAILURE_LIMIT: u32 = 3;

/// Parse one `--until` word into the state it names.
///
/// `unknown` is deliberately not spellable: it is *departure* — a record
/// whose state was withdrawn — not a state to wait for, and it is also the
/// open-enum decode of any newer vocabulary word, so waiting on it would mean
/// "wait for something this build cannot name".
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
    /// The `phux.agent/v1` key was deleted, or its value stopped parsing as a
    /// record with a non-empty `name` (L3 §3.7 reads both as "no declared
    /// agent").
    Tombstone,
    /// The record survived but its state withdrew to `unknown` — either
    /// explicitly cleared, or replaced by a vocabulary this build cannot
    /// name.
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
    /// Nothing decided: no state change, or a change to a state nobody asked
    /// for.
    Pending,
    /// An observed transition **into** a target state. The only thing that
    /// satisfies a wait.
    Satisfied {
        /// The state held before this observation.
        from: AgentMetaState,
        /// The state observed now.
        to: AgentMetaState,
    },
    /// The agent went away mid-wait. Distinct from both success and timeout:
    /// a caller must not read it as "the turn finished".
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
    /// The `GET_METADATA` poll floor, recovering an edge the push half never
    /// delivered (dropped notification, or two transitions inside one
    /// detector tick).
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
/// wait is satisfied.
///
/// Pulled out of the async driver so the predicate — the whole point of this
/// module — is unit-testable with no server, no sockets, and no clock.
#[derive(Debug)]
pub struct EdgeTracker {
    targets: Vec<AgentMetaState>,
    baseline: AgentMetaState,
    last: AgentMetaState,
    edges: u32,
}

impl EdgeTracker {
    /// Seed a tracker with the pre-wait `baseline` level and the states the
    /// caller is waiting for.
    ///
    /// The baseline is *recorded*, never *evaluated*: a tracker built with
    /// `baseline = Idle` and `targets = [Idle]` is not satisfied, and there
    /// is no code path that could make it so. That is the corpse rule.
    #[must_use]
    pub fn new(baseline: AgentMetaState, targets: &[AgentMetaState]) -> Self {
        Self {
            targets: targets.to_vec(),
            baseline,
            last: baseline,
            edges: 0,
        }
    }

    /// Fold one observation of the pane's record into the wait.
    ///
    /// `None` means the record is gone (deleted, or a value that no longer
    /// reads as a record) — a departure, never a completion.
    pub fn observe(&mut self, record: Option<&AgentRecord>) -> Verdict {
        let Some(record) = record else {
            return Verdict::Departed {
                from: self.last,
                reason: DepartureReason::Tombstone,
            };
        };
        let next = record.state;
        if next == self.last {
            // The poll floor re-reads the same level several times a second;
            // repeating a level is not an edge.
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
    /// The pane declared no `phux.agent/v1` record at subscribe time, so
    /// there is no lifecycle to wait on. Refused up front rather than waited
    /// out: a pane with no record never publishes an edge, so the wait would
    /// be an elaborate way to spend the whole timeout.
    #[error("pane has no phux.agent/v1 record, so there is no agent lifecycle to wait on")]
    NoRecord,
    /// The agent went away mid-wait. **Not** success and **not** a timeout.
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

/// Read a pane's `phux.agent/v1` record over `conn`.
///
/// `None` is "no declared agent" — the key is unset, or its value does not
/// read as a record with a non-empty `name` (L3 §3.7) — never an error.
async fn read_record(
    conn: &mut Connection,
    terminal: &TerminalId,
    request_id: u32,
) -> Result<(Option<AgentRecord>, Vec<FrameKind>), AttachError> {
    let (answer, interleaved) = conn
        .request_metadata(
            request_id,
            Scope::Terminal(terminal.clone()),
            TERMINAL_AGENT_KEY.to_owned(),
        )
        .await?
        .into_parts();
    let value = answer.map_err(|refusal| AttachError::Refused(refusal.to_string()))?;
    Ok((value.as_deref().and_then(parse_agent_record), interleaved))
}

/// Read a pane's `phux.agent/v1` record on a fresh connection — the poll
/// floor's single read.
///
/// Side-effect-free in the same sense as `GET_SCREEN`: it neither attaches
/// nor resizes, so polling a pane someone is using disturbs nothing.
///
/// # Errors
///
/// Propagates [`AttachError`] from connect, transport, or a server refusal.
pub async fn fetch_agent_record(
    socket: &Path,
    terminal: &TerminalId,
) -> Result<Option<AgentRecord>, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    let (record, _interleaved) = read_record(&mut conn, terminal, BASELINE_REQUEST_ID).await?;
    // This connection subscribed to nothing, so the server pushes nothing to
    // it ahead of the answer; an interleaved frame here would be a server
    // bug, and dropping it loses no transition the wait needs.
    Ok(record)
}

/// The record an interleaved `METADATA_CHANGED` carries for `terminal`, or
/// `None` if the frame is about something else.
///
/// The inner `Option` is the record itself: `Some(None)` is a tombstone.
#[allow(
    clippy::option_option,
    reason = "the outer Option answers 'is this frame ours', the inner one \
              'record or tombstone'; collapsing them would erase the \
              tombstone, which is the one observation the wait must not miss"
)]
fn record_from_frame(frame: &FrameKind, terminal: &TerminalId) -> Option<Option<AgentRecord>> {
    let FrameKind::MetadataChanged { scope, key, value } = frame else {
        return None;
    };
    if key != TERMINAL_AGENT_KEY {
        return None;
    }
    let Scope::Terminal(id) = scope else {
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
type Decision = (Verdict, Option<AgentRecord>, EdgeSource);

/// Wait until `terminal`'s agent record transitions **into** one of
/// `targets`, or `timeout` elapses.
///
/// The predicate, precisely. Let `O_0, O_1, …` be this call's observations of
/// the pane's `phux.agent/v1` record, in arrival order, merged from the
/// `METADATA_CHANGED` push stream and the `GET_METADATA` poll floor, starting
/// with the pre-wait baseline read. The wait is **satisfied** at the first
/// `i > 0` such that `O_i` is a record whose state differs from the state
/// last held and is a member of `targets`. It **fails** with
/// [`AgentWaitError::Departed`] at the first `O_i` that is a tombstone, or
/// whose state differs from the last held and is `unknown`. No single
/// observation, at any index — least of all `O_0` — satisfies the wait on its
/// own. A pane resting at `idle` therefore never satisfies `--until idle`
/// without first having been observed in some other state, which is exactly
/// what stops a wait from succeeding on a pane whose agent crashed.
///
/// # Errors
///
/// [`AgentWaitError::NoRecord`] when the pane declares no record at subscribe
/// time; [`AgentWaitError::Departed`] when the agent goes away mid-wait;
/// [`AgentWaitError::Transport`] on connect/transport failure.
#[allow(
    clippy::too_many_lines,
    reason = "one function so the subscribe-then-read ordering, the two \
              halves, and the deadline are readable as a single sequence"
)]
#[allow(
    clippy::future_not_send,
    reason = "the push and poll halves share one EdgeTracker through a \
              RefCell because they are polled by one task; ADR-0003 binds \
              the CLI to a current-thread runtime"
)]
pub async fn wait_for_agent_state(
    socket: &Path,
    terminal: &TerminalId,
    targets: &[AgentMetaState],
    timeout: Option<Duration>,
    poll_interval: Duration,
) -> Result<AgentWaitResult, AgentWaitError> {
    // Subscribe FIRST, and read the baseline on the same connection, so the
    // window between "what is it now" and "tell me when it changes" does not
    // exist. See the module docs.
    let mut conn = subscribe(socket, Some(terminal.clone())).await?;
    let (answered, interleaved) = read_record(&mut conn, terminal, BASELINE_REQUEST_ID).await?;

    // Time order: anything the server published between the subscribe and the
    // answer, then the answer itself. Folding the interleave in rather than
    // dropping it is what keeps a `working -> idle -> working` flicker inside
    // that window from being invisible.
    let mut observations: Vec<Option<AgentRecord>> = interleaved
        .iter()
        .filter_map(|frame| record_from_frame(frame, terminal))
        .collect();
    observations.push(answered);

    let mut sequence = observations.into_iter();
    // `push` above guarantees at least one element.
    let baseline_record = sequence.next().flatten();
    let Some(baseline_record) = baseline_record else {
        return Err(AgentWaitError::NoRecord);
    };
    let mut tracker = EdgeTracker::new(baseline_record.state, targets);
    let mut latest = Some(baseline_record);

    // Replay anything that arrived inside the subscribe/read window.
    for observation in sequence {
        let verdict = tracker.observe(observation.as_ref());
        if observation.is_some() {
            latest.clone_from(&observation);
        }
        match verdict {
            Verdict::Pending => {}
            Verdict::Satisfied { from, to } => {
                return Ok(AgentWaitResult {
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
                });
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

    let tracker = RefCell::new(tracker);
    let stats = RefCell::new(Stats::default());
    let latest = RefCell::new(latest);
    let push_ended = Cell::new(false);

    let push_half = async {
        let mut decided: Option<Decision> = None;
        let streamed = stream_items(&mut conn, |item| {
            let WatchItem::AgentState(update) = item else {
                return true;
            };
            {
                let mut stats = stats.borrow_mut();
                stats.pushes = stats.pushes.saturating_add(1);
            }
            let verdict = tracker.borrow_mut().observe(update.record.as_ref());
            if update.record.is_some() {
                latest.borrow_mut().clone_from(&update.record);
            }
            if matches!(verdict, Verdict::Pending) {
                return true;
            }
            decided = Some((verdict, update.record, EdgeSource::Push));
            false
        })
        .await;
        push_ended.set(true);
        // A stream that ends without deciding is not fatal: the poll floor is
        // the floor precisely so a dropped subscription degrades to latency
        // rather than to a wrong answer.
        let _ = streamed;
        match decided {
            Some(decision) => Ok(decision),
            None => std::future::pending::<Result<Decision, AgentWaitError>>().await,
        }
    };

    let poll_half = async {
        let mut failures: u32 = 0;
        loop {
            tokio::time::sleep(poll_interval).await;
            match fetch_agent_record(socket, terminal).await {
                Ok(record) => {
                    failures = 0;
                    {
                        let mut stats = stats.borrow_mut();
                        stats.polls = stats.polls.saturating_add(1);
                    }
                    let verdict = tracker.borrow_mut().observe(record.as_ref());
                    if record.is_some() {
                        latest.borrow_mut().clone_from(&record);
                    }
                    if !matches!(verdict, Verdict::Pending) {
                        return Ok((verdict, record, EdgeSource::Poll));
                    }
                }
                Err(err) => {
                    failures = failures.saturating_add(1);
                    if failures >= POLL_FAILURE_LIMIT && push_ended.get() {
                        return Err(AgentWaitError::Transport(err));
                    }
                }
            }
        }
    };

    let deadline = async {
        match timeout {
            Some(limit) => tokio::time::sleep(limit).await,
            None => std::future::pending::<()>().await,
        }
    };

    let decision: Option<Decision> = tokio::select! {
        decided = push_half => Some(decided?),
        decided = poll_half => Some(decided?),
        () = deadline => None,
    };

    let stats = *stats.borrow();
    let tracker = tracker.into_inner();
    let latest = latest.into_inner();
    match decision {
        Some((Verdict::Satisfied { from, to }, record, via)) => Ok(AgentWaitResult {
            edge: Some(ObservedEdge { from, to, via }),
            baseline: tracker.baseline(),
            last: tracker.last(),
            record: record.or(latest),
            edges: tracker.edges(),
            polls: stats.polls,
            pushes: stats.pushes,
        }),
        Some((Verdict::Departed { from, reason }, record, _)) => Err(AgentWaitError::Departed {
            from,
            reason,
            last_record: record.or(latest),
        }),
        // `Verdict::Pending` never decides — the two halves only return on a
        // decided verdict — so this folds in with the deadline arm rather
        // than being an unreachable panic.
        Some((Verdict::Pending, _, _)) | None => Ok(AgentWaitResult {
            edge: None,
            baseline: tracker.baseline(),
            last: tracker.last(),
            record: latest,
            edges: tracker.edges(),
            polls: stats.polls,
            pushes: stats.pushes,
        }),
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
    #![allow(
        clippy::future_not_send,
        reason = "the wait it drives is !Send by design; ADR-0003 binds the \
                  CLI to a current-thread runtime"
    )]

    use tokio::net::UnixListener;

    use crate::testkit::{EndOfScript, ScriptSpec, ScriptedServer};

    use super::*;

    fn record(state: &str) -> Vec<u8> {
        format!(r#"{{"name":"reviewer","kind":"claude","state":"{state}"}}"#).into_bytes()
    }

    fn changed(pane: &TerminalId, value: Option<Vec<u8>>) -> FrameKind {
        FrameKind::MetadataChanged {
            scope: Scope::Terminal(pane.clone()),
            key: TERMINAL_AGENT_KEY.to_owned(),
            value,
        }
    }

    fn state_record(state: AgentMetaState) -> AgentRecord {
        AgentRecord {
            name: "reviewer".to_owned(),
            state,
            ..AgentRecord::default()
        }
    }

    /// Drive a real `wait_for_agent_state` against the shared scripted
    /// server. `level` is what every `GET_METADATA` answers with; `pushes`
    /// are the `METADATA_CHANGED` frames the server fans out to the *first*
    /// connection once its metadata subscription registers.
    ///
    /// Every accepted connection is served on its own task because the wait
    /// holds the subscribed connection open for its whole life while the
    /// poll floor dials a fresh one per read — a serial accept loop would
    /// wedge behind the long-lived one and starve the floor.
    async fn drive(
        level: Option<Vec<u8>>,
        pushes: Vec<FrameKind>,
        targets: &[AgentMetaState],
        timeout: Duration,
    ) -> Result<AgentWaitResult, AgentWaitError> {
        let pane = TerminalId::local(7);
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        let server = tokio::spawn(async move {
            let mut first = true;
            loop {
                let (stream, _) = listener.accept().await.expect("accept scripted client");
                let level = level.clone();
                let mut spec = ScriptSpec::new().metadata(move |_scope, key| {
                    if key == TERMINAL_AGENT_KEY {
                        level.clone()
                    } else {
                        None
                    }
                });
                if first {
                    spec = spec.extend(pushes.clone());
                    first = false;
                }
                // Stay up: a hang-up would end the wait for the wrong
                // reason, and the corpse test needs a *live* server that
                // keeps answering `idle` for the whole deadline.
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

    /// THE test. A pane resting at `idle` — the detector's fail-safe
    /// fallthrough, which no shipped manifest asserts positively, and which
    /// is exactly what a crashed agent's pane reads — must NOT satisfy
    /// `--until idle`. A level read is not an edge.
    #[test]
    fn a_level_read_of_idle_never_satisfies_the_wait() {
        let mut tracker = EdgeTracker::new(AgentMetaState::Idle, &[AgentMetaState::Idle]);
        // The baseline itself is never evaluated.
        assert_eq!(tracker.last(), AgentMetaState::Idle);
        // And re-reading the same level, forever, is still not an edge — this
        // is precisely what the poll floor does to a corpse.
        for _ in 0..100 {
            assert_eq!(
                tracker.observe(Some(&state_record(AgentMetaState::Idle))),
                Verdict::Pending,
                "a repeated level read must never satisfy a completion gate"
            );
        }
        assert_eq!(tracker.edges(), 0);
    }

    /// The transition the wait exists for: whatever was asserting `working`
    /// stopped asserting it.
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

    /// `blocked` is asserted positively by five shipped manifest rules — but
    /// a pane that was *already* blocked when the wait began still has to
    /// transition to satisfy the gate. The level is not the edge, whatever
    /// the state.
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

    /// A tombstone ends the wait as a departure, never as success — the
    /// agent went away, which is not the same statement as "it finished".
    #[test]
    fn a_tombstone_is_a_departure_not_a_completion() {
        let mut tracker = EdgeTracker::new(AgentMetaState::Working, DEFAULT_UNTIL);
        assert_eq!(
            tracker.observe(None),
            Verdict::Departed {
                from: AgentMetaState::Working,
                reason: DepartureReason::Tombstone,
            }
        );
    }

    /// A state that withdraws to `unknown` — cleared, or replaced by a
    /// vocabulary this build cannot name — is departure too, not an edge
    /// into anything waitable.
    #[test]
    fn a_withdrawal_to_unknown_is_a_departure() {
        let mut tracker = EdgeTracker::new(AgentMetaState::Working, DEFAULT_UNTIL);
        assert_eq!(
            tracker.observe(Some(&state_record(AgentMetaState::Unknown))),
            Verdict::Departed {
                from: AgentMetaState::Working,
                reason: DepartureReason::WithdrewToUnknown,
            }
        );
    }

    /// `unknown` is not spellable as a `--until` target, and neither is a
    /// word from a newer vocabulary.
    #[test]
    fn until_vocabulary_is_closed_and_excludes_unknown() {
        assert_eq!(parse_until("idle"), Some(AgentMetaState::Idle));
        assert_eq!(parse_until("working"), Some(AgentMetaState::Working));
        assert_eq!(parse_until("blocked"), Some(AgentMetaState::Blocked));
        assert_eq!(parse_until("done"), Some(AgentMetaState::Done));
        assert_eq!(parse_until("unknown"), None);
        assert_eq!(parse_until("hibernating"), None);
        assert_eq!(parse_until(""), None);
    }

    /// The default set is the three ways a turn can end, and never
    /// `working`.
    #[test]
    fn default_until_set_is_the_three_end_states() {
        assert_eq!(
            DEFAULT_UNTIL,
            &[
                AgentMetaState::Idle,
                AgentMetaState::Blocked,
                AgentMetaState::Done
            ]
        );
        assert!(!DEFAULT_UNTIL.contains(&AgentMetaState::Working));
    }

    /// An interleaved `METADATA_CHANGED` for another key, another scope, or
    /// another Terminal is not one of ours.
    #[test]
    fn interleaved_frames_are_filtered_to_this_terminals_agent_key() {
        let pane = TerminalId::local(7);
        let other = TerminalId::local(8);
        assert!(record_from_frame(&changed(&pane, Some(record("idle"))), &pane).is_some());
        assert!(record_from_frame(&changed(&other, Some(record("idle"))), &pane).is_none());
        assert!(
            record_from_frame(
                &FrameKind::MetadataChanged {
                    scope: Scope::Terminal(pane.clone()),
                    key: "phux.tags/v1".to_owned(),
                    value: Some(record("idle")),
                },
                &pane,
            )
            .is_none()
        );
        // A tombstone for our key IS one of ours, carrying `None`.
        assert_eq!(record_from_frame(&changed(&pane, None), &pane), Some(None));
    }

    /// End to end against the scripted server: a pane whose record answers
    /// `idle` on every read and never publishes a change must TIME OUT, not
    /// succeed. This is the corpse case with real frames on a real socket.
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

    /// End to end: an observed `working -> blocked` transition satisfies the
    /// wait, and the result names the edge, the source, and the identity the
    /// record carried.
    #[tokio::test]
    async fn an_observed_transition_satisfies_the_wait_end_to_end() {
        let pane = TerminalId::local(7);
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
        // Provenance survives into the result: the caller can see *which*
        // agent settled, not just that something did.
        assert_eq!(
            outcome
                .record
                .expect("a satisfied wait carries a record")
                .name,
            "reviewer"
        );
    }

    /// A record that goes away mid-wait ends it with a typed departure — the
    /// distinct error, not a hang to the deadline and not a success.
    #[tokio::test]
    async fn a_tombstone_mid_wait_is_a_typed_departure_end_to_end() {
        let pane = TerminalId::local(7);
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

    /// A pane with no record at all is refused up front rather than waited
    /// out: it can never publish an edge.
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
}
