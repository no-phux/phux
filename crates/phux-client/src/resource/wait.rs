//! `phux resource wait` (PHA-406 D2): wait for one resource's process to end.
//!
//! Race-free and resumable, composed from the event journal and a level read
//! rather than a server primitive (ADR-0123, ADR-0124).
//!
//! Each connection runs the same steps, in order, on that one connection:
//!
//! 1. `SUBSCRIBE_EVENTS { Some(id), after_seq }`: the cursor's `seq` when it
//!    belongs to this server incarnation, else journal semantics with no
//!    replay (`2^64 - 1`) on a server with the journal.
//! 2. `GET_STATE`. Present with an exit facet: `exited`. Present and live:
//!    wait for `terminal_control { Exited }` or `pane_closed` for that id.
//!    Absent: `gone`, unless an event reported the close.
//! 3. A cursor replay is pumped as the connection takes it, so the state
//!    answer can overtake it. An absence is trusted only once the replay has
//!    reached the snapshot's journal head (L1 §7.3), which is this
//!    connection's: the newest `seq` its subscription admits, so a head at or
//!    below the cursor is caught up at once. A gap during the replay covers
//!    the range the journal no longer holds, and the answer says evidence
//!    was lost.
//! 4. After the replay, or past the head, a `journal_gap` is a live loss
//!    whose events may still be in the ring, and a subscription never re-delivers what it reported
//!    missing: the wait resumes on a fresh connection from the last `seq` it
//!    accounted for. A `source_gap` or an exit notice re-reads the level.
//! 5. The caller's [`Deadline`] bounds everything, connect included. The
//!    cursor reached rides the answer and the error alike, so a wait cut
//!    short resumes on another connection.
//!
//! Race-freedom: the subscription is registered before the snapshot is cut,
//! so an exit after the cut arrives as an event, and an exit before it is in
//! the snapshot (a retained exit facet), in the replay (a close the journal
//! still holds), or in its absence (not retained, an honest `gone`).
//! Re-running the wait is idempotent: its answer is a level read of retained
//! state.

use std::path::Path;
use std::time::Duration;

use phux_client_runtime::reconnect::Ladder;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    AgentEvent, CommandResult, ErrorCode, EventStamp, FrameKind, ResourceLifecycle,
};
use phux_protocol::wire::info::{ExitFacet, ResourceInfo};
use serde_json::{Value, json};

use super::cursor::{Cursor, NO_REPLAY, ResumeState};
use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::deadline::Deadline;
use crate::selector::format_terminal_id;
use crate::state::{StateView, get_state_reply, state_view};

/// `schema_version` of the `resource wait --json` document.
pub const WAIT_SCHEMA_VERSION: u32 = 1;

/// How a resource wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The process ended; [`ResourceWait::exit`] says how, as far as known.
    Exited,
    /// The resource is not in the inventory and no event reported its end:
    /// it closed before the wait began and was not retained, or never
    /// existed.
    Gone,
    /// The deadline passed first.
    TimedOut,
}

impl WaitOutcome {
    /// The name the `--json` document uses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::Gone => "gone",
            Self::TimedOut => "timed_out",
        }
    }
}

/// How the process ended. A fact the client could not learn is `None`,
/// never a guess.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExitReport {
    /// `_exit(n)` status.
    pub status: Option<i32>,
    /// Terminating signal.
    pub signal: Option<i32>,
    /// Why the process ended, in the `RESOURCE_CLOSED.reason` vocabulary.
    pub reason: Option<&'static str>,
    /// When it ended, Unix milliseconds.
    pub exited_at_ms: Option<u64>,
}

impl ExitReport {
    const fn from_facet(facet: &ExitFacet) -> Self {
        Self {
            status: facet.exit_status,
            signal: facet.signal,
            reason: super::close_reason_name(facet.reason),
            exited_at_ms: Some(facet.exited_at_ms),
        }
    }

    fn from_event(
        status: Option<i32>,
        reason: Option<&'static str>,
        stamp: Option<&EventStamp>,
    ) -> Self {
        Self {
            status,
            signal: None,
            reason,
            exited_at_ms: stamp.map(|stamp| stamp.ts_ms),
        }
    }

    /// The `exit` object of the `--json` document.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "status": self.status,
            "signal": self.signal,
            "reason": self.reason,
            "exited_at_ms": self.exited_at_ms,
        })
    }
}

/// What a resource wait answered.
#[derive(Debug, Clone)]
pub struct ResourceWait {
    /// The resource waited on.
    pub resource: ResourceId,
    /// How the wait ended.
    pub outcome: WaitOutcome,
    /// How the process ended, when it did.
    pub exit: Option<ExitReport>,
    /// Whether the resource is still in the inventory, retained after its
    /// exit (ADR-0124).
    pub retained: bool,
    /// Wall time from the start of the deadline.
    pub waited: Duration,
    /// Where to resume: pass it back as the next wait's cursor. `None` when
    /// the server keeps no journal.
    pub cursor: Option<Cursor>,
    /// The caller's cursor belonged to another server incarnation (or a
    /// server with no journal), so the wait fell back to its level read.
    pub cursor_void: bool,
    /// The replay reported a range the journal had already evicted, so a
    /// close in that range was never seen: a `gone` may hide an exit.
    pub evidence_lost: bool,
}

impl ResourceWait {
    /// The `resource wait --json` document, shared by the CLI and MCP.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "schema_version": WAIT_SCHEMA_VERSION,
            "resource": format_terminal_id(&self.resource),
            "outcome": self.outcome.as_str(),
            "exit": self.exit.as_ref().map(ExitReport::to_json),
            "retained": self.retained,
            "waited_ms": u64::try_from(self.waited.as_millis()).unwrap_or(u64::MAX),
            "cursor": self.cursor.as_ref().map(ToString::to_string),
            "evidence_lost": self.evidence_lost,
        })
    }
}

/// Why a resource wait could not answer.
#[derive(Debug, thiserror::Error)]
pub enum WaitFailure {
    /// The connection or a request failed.
    #[error(transparent)]
    Attach(#[from] AttachError),
    /// The resource is absent from a partial view of the fleet (a federation
    /// satellite did not answer), so "gone" would be a guess.
    #[error(
        "the resource is not in this server's view, but the view is incomplete ({}); \
         it may be on an unreachable satellite",
        .0.join("; ")
    )]
    PartialView(Vec<String>),
    /// The server refused the event subscription or the state read: a
    /// scoped connection without `OBSERVE` on the resource (workload-auth
    /// §7). No answer would ever arrive, so the wait ends here instead of at
    /// its deadline.
    #[error("the server refused the wait: {0}")]
    Denied(String),
}

/// A wait that could not answer, with the cursor it had reached, so a
/// caller can still resume it against the same server incarnation.
#[derive(Debug, thiserror::Error)]
#[error("{cause}")]
pub struct ResourceWaitError {
    /// What went wrong.
    pub cause: WaitFailure,
    /// The position the wait had reached; `None` on a server with no
    /// journal.
    pub cursor: Option<Cursor>,
}

/// Wait until `resource`'s process ends, the deadline passes, or the answer
/// is `gone`. `after` resumes from a previous wait's cursor.
///
/// # Errors
///
/// [`ResourceWaitError`] on a transport failure before the answer, a
/// refused subscription, or an absence in a partial fleet view; it carries
/// the cursor reached.
pub async fn wait_for_exit(
    socket: &Path,
    resource: ResourceId,
    after: Option<&Cursor>,
    deadline: Deadline,
) -> Result<ResourceWait, ResourceWaitError> {
    let mut progress = Progress::new(resource, after.cloned());
    let run = deadline.run(drive(socket, &mut progress)).await;
    let waited = deadline.started_at().elapsed();
    match run.transpose() {
        Ok(verdict) => Ok(progress.finish(verdict, waited)),
        Err(cause) => Err(ResourceWaitError {
            cause,
            cursor: progress.resume.cursor(),
        }),
    }
}

/// The answer, short of the bookkeeping [`ResourceWait`] adds.
#[derive(Debug)]
struct Verdict {
    outcome: WaitOutcome,
    exit: Option<ExitReport>,
    retained: bool,
}

impl Verdict {
    const fn gone() -> Self {
        Self {
            outcome: WaitOutcome::Gone,
            exit: None,
            retained: false,
        }
    }

    const fn timed_out() -> Self {
        Self {
            outcome: WaitOutcome::TimedOut,
            exit: None,
            retained: false,
        }
    }
}

/// What one frame means for the wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    /// Nothing the wait acts on.
    Nothing,
    /// Level state may have moved in ways the stream cannot show (a source
    /// gap), or the process exited and the exit facet is worth reading.
    Reread,
    /// The resource closed: the answer is in hand.
    Closed,
    /// A live `journal_gap`: resume on a fresh connection.
    Reconnect,
}

/// Where one connection's wait stands after a step.
#[derive(Debug)]
enum Step {
    /// The answer.
    Answer(Verdict),
    /// The resource runs: wait for its events.
    Watch,
    /// The resource is absent but the replay has not reached the cut yet.
    CatchUp,
    /// A live gap: resume on a fresh connection.
    Reconnect,
}

/// Whether this connection's subscription is still replaying from a cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Pulling a cursor replay: a gap is the journal's eviction, and an
    /// absence is not trusted until the replay reaches the cut.
    Replaying,
    /// Live: a gap is a loss this subscription cannot recover.
    Live,
}

/// Everything the wait has learned, kept outside the deadline-bounded future
/// so a timeout or an error still reports the cursor it reached.
#[derive(Debug)]
struct Progress {
    resource: ResourceId,
    resume: ResumeState,
    seen_exit: Option<ExitReport>,
    closed: bool,
    denied: Option<String>,
    /// This connection's subscription phase.
    phase: Phase,
    /// Every `seq` at or below this was delivered to this connection's
    /// replay or reported missing.
    covered: u64,
    /// The journal head at this connection's latest snapshot cut.
    head: Option<u64>,
    /// A replay gap covered part of the waited range.
    evidence_lost: bool,
}

impl Progress {
    const fn new(resource: ResourceId, after: Option<Cursor>) -> Self {
        Self {
            resource,
            resume: ResumeState::new(after),
            seen_exit: None,
            closed: false,
            denied: None,
            phase: Phase::Live,
            covered: 0,
            head: None,
            evidence_lost: false,
        }
    }

    /// Plan this connection's subscription and return its `after_seq`.
    fn begin(&mut self, conn: &Connection) -> Option<u64> {
        let after_seq = self.resume.bind(conn);
        let replay_from = after_seq.filter(|seq| *seq != NO_REPLAY);
        self.phase = if replay_from.is_some() {
            Phase::Replaying
        } else {
            Phase::Live
        };
        self.covered = replay_from.unwrap_or(0);
        self.head = None;
        after_seq
    }

    /// The `seq` the wait has accounted for, if any.
    fn position(&self) -> Option<u64> {
        self.resume.cursor().map(|cursor| cursor.seq())
    }

    /// The subscription's refusal, once the server sent one.
    fn refusal(&self) -> Result<(), WaitFailure> {
        self.denied
            .clone()
            .map_or(Ok(()), |message| Err(WaitFailure::Denied(message)))
    }

    fn observe(&mut self, frame: &FrameKind) -> Signal {
        if let Some(message) = denial(frame) {
            self.denied = Some(message.to_owned());
            return Signal::Nothing;
        }
        let FrameKind::Event {
            terminal,
            event,
            stamp,
        } = frame
        else {
            return Signal::Nothing;
        };
        let stamp = stamp.as_deref();
        if let Some(stamp) = stamp {
            self.advance(stamp.seq);
        }
        if let AgentEvent::JournalGap { last_missing, .. } = event {
            return self.on_gap(*last_missing);
        }
        if terminal.as_ref() != Some(&self.resource) {
            return Signal::Nothing;
        }
        self.observe_own(event, stamp)
    }

    /// Account for everything through `seq`.
    fn advance(&mut self, seq: u64) {
        self.resume.note(seq);
        if self.phase == Phase::Replaying {
            self.covered = self.covered.max(seq);
            self.settle();
        }
    }

    /// A gap during the replay, up to the head, is the journal's eviction:
    /// the range is as accounted for as it will ever be, and its events are
    /// lost. A live gap, or one past the head, asks for a reconnect.
    fn on_gap(&mut self, last_missing: u64) -> Signal {
        let past_cut = self.head.is_some_and(|head| last_missing > head);
        if self.phase == Phase::Live || past_cut {
            return Signal::Reconnect;
        }
        self.evidence_lost = true;
        self.advance(last_missing);
        Signal::Nothing
    }

    /// Leave the replay once it has covered the snapshot's cut.
    fn settle(&mut self) {
        if self.head.is_some_and(|head| self.covered >= head) {
            self.phase = Phase::Live;
        }
    }

    /// Take the snapshot's cut. A peer that names no head leaves nothing to
    /// catch up to: its answer is the whole truth, as before heads existed.
    fn cut(&mut self, head: Option<u64>) {
        self.head = head;
        match head {
            None => self.phase = Phase::Live,
            Some(_) => self.settle(),
        }
        if self.phase == Phase::Live
            && let Some(head) = head
        {
            // The level read accounts for everything up to the cut.
            self.resume.note(head);
        }
    }

    fn observe_own(&mut self, event: &AgentEvent, stamp: Option<&EventStamp>) -> Signal {
        match event {
            AgentEvent::SourceGap { .. } => Signal::Reread,
            AgentEvent::TerminalControl {
                lifecycle: ResourceLifecycle::Exited,
                exit_status,
                ..
            } => {
                self.seen_exit = Some(ExitReport::from_event(*exit_status, Some("exited"), stamp));
                Signal::Reread
            }
            AgentEvent::ResourceClosed { exit_status } => {
                self.seen_exit
                    .get_or_insert_with(|| ExitReport::from_event(*exit_status, None, stamp));
                self.closed = true;
                Signal::Closed
            }
            _ => Signal::Nothing,
        }
    }

    /// The step a level read leads to.
    fn classify(&self, view: &StateView) -> Result<Step, WaitFailure> {
        if self.closed {
            return Ok(Step::Answer(self.ended(false)));
        }
        match super::find(view.snapshot(), &self.resource) {
            Some(info) if has_exited(info) => Ok(Step::Answer(self.retained_exit(info))),
            Some(_) => Ok(Step::Watch),
            None if self.seen_exit.is_some() => Ok(Step::Answer(self.ended(false))),
            None if !view.is_complete() => Err(WaitFailure::PartialView(
                view.degradation().notices().to_vec(),
            )),
            None if self.phase == Phase::Replaying => Ok(Step::CatchUp),
            None => Ok(Step::Answer(Verdict::gone())),
        }
    }

    fn retained_exit(&self, info: &ResourceInfo) -> Verdict {
        let exit = info
            .exit
            .as_ref()
            .map(ExitReport::from_facet)
            .or_else(|| self.seen_exit.clone());
        Verdict {
            outcome: WaitOutcome::Exited,
            exit,
            retained: true,
        }
    }

    fn ended(&self, retained: bool) -> Verdict {
        Verdict {
            outcome: WaitOutcome::Exited,
            exit: self.seen_exit.clone(),
            retained,
        }
    }

    /// The answer an absence gives once the replay has reached the cut.
    fn absent(&self) -> Verdict {
        if self.seen_exit.is_some() {
            self.ended(false)
        } else {
            Verdict::gone()
        }
    }

    /// An exit or close the wait saw but could not confirm with a level read
    /// before its deadline still answers `exited`.
    fn observed_end(&self) -> Option<Verdict> {
        (self.closed || self.seen_exit.is_some()).then(|| self.ended(!self.closed))
    }

    fn finish(self, verdict: Option<Verdict>, waited: Duration) -> ResourceWait {
        let verdict = verdict
            .or_else(|| self.observed_end())
            .unwrap_or_else(Verdict::timed_out);
        ResourceWait {
            cursor: self.resume.cursor(),
            cursor_void: self.resume.cursor_void(),
            evidence_lost: self.evidence_lost,
            resource: self.resource,
            outcome: verdict.outcome,
            exit: verdict.exit,
            retained: verdict.retained,
            waited,
        }
    }
}

/// The message of an uncorrelated `PERMISSION_DENIED`: how a scoped server
/// answers a subscription frame it refuses, since `SUBSCRIBE_EVENTS` has no
/// reply of its own. A hub's degradation notices carry other codes.
const fn denial(frame: &FrameKind) -> Option<&String> {
    match frame {
        FrameKind::Error {
            request_id: None,
            code: ErrorCode::PermissionDenied,
            message,
        } => Some(message),
        _ => None,
    }
}

fn has_exited(info: &ResourceInfo) -> bool {
    info.exit.is_some() || info.lifecycle == ResourceLifecycle::Exited
}

/// The pause between reconnects that made no progress: the runtime's
/// agent-verb ladder (ADR-0133), 50 ms doubling to 1 s. An agent is
/// blocking on this verb over a local socket, so the interactive lane's
/// 500 ms floor would be latency it sees for no radio saved; the lane is
/// named here rather than forked so the arithmetic exists once.
const RECONNECT_LADDER: Ladder = Ladder::AGENT_VERB;

/// Run connections until one answers; a live gap ends a connection and the
/// next resumes from the position reached. A reconnect that did not move
/// the position backs off, so a gap that repeats on every connection waits
/// out the deadline instead of spinning.
async fn drive(socket: &Path, progress: &mut Progress) -> Result<Verdict, WaitFailure> {
    let mut pause = RECONNECT_LADDER.floor;
    loop {
        let before = progress.position();
        if let Some(verdict) = session(socket, progress).await? {
            return Ok(verdict);
        }
        if progress.position() == before {
            tokio::time::sleep(pause).await;
            pause = RECONNECT_LADDER.next(pause);
        } else {
            pause = RECONNECT_LADDER.floor;
        }
    }
}

/// One connection: subscribe from the current position, read the level,
/// and watch. `None` asks for a fresh connection.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the connection is the subscription: it lives until the answer"
)]
async fn session(socket: &Path, progress: &mut Progress) -> Result<Option<Verdict>, WaitFailure> {
    let mut conn = Connection::connect(socket).await?;
    let after_seq = progress.begin(&conn);
    conn.send(&FrameKind::SubscribeEvents {
        terminal: Some(progress.resource.clone()),
        after_seq,
    })
    .await?;
    loop {
        match level_step(&mut conn, progress).await? {
            Step::Answer(verdict) => return Ok(Some(verdict)),
            Step::Reconnect => return Ok(None),
            Step::Watch | Step::CatchUp => {}
        }
        match await_change(&mut conn, progress).await? {
            Signal::Closed => return Ok(Some(progress.ended(false))),
            Signal::Reconnect => return Ok(None),
            Signal::Nothing | Signal::Reread => {}
        }
    }
}

/// A level read, followed by the replay catch-up an absence needs.
async fn level_step(conn: &mut Connection, progress: &mut Progress) -> Result<Step, WaitFailure> {
    match level_read(conn, progress).await? {
        Step::CatchUp => catch_up(conn, progress).await,
        step => Ok(step),
    }
}

/// `GET_STATE`: take the snapshot's cut first, so each event the server
/// interleaved ahead of the ack is judged against the head, then fold those
/// events in. A refusal still reads them, since a denial is the answer.
async fn level_read(conn: &mut Connection, progress: &mut Progress) -> Result<Step, WaitFailure> {
    let (result, interleaved) = get_state_reply(conn).await?;
    let refused = command_denial(&result);
    let view = state_view(conn, result, &interleaved);
    if let Ok(view) = &view {
        progress.cut(view.snapshot().journal_head());
    }
    let mut reconnect = false;
    for frame in &interleaved {
        reconnect |= progress.observe(frame) == Signal::Reconnect;
    }
    if let Some(message) = refused {
        progress.denied.get_or_insert(message);
    }
    progress.refusal()?;
    let view = view?;
    if reconnect {
        return Ok(Step::Reconnect);
    }
    progress.classify(&view)
}

/// A `PERMISSION_DENIED` answer to the state read: a scoped workload that
/// may not read the inventory.
fn command_denial(result: &CommandResult) -> Option<String> {
    match result {
        CommandResult::Error {
            code: ErrorCode::PermissionDenied,
            message,
        } => Some(message.clone()),
        _ => None,
    }
}

/// Consume the replay until it reaches the snapshot's cut: a close in it is
/// the answer, and its absence makes the snapshot's absence honest.
async fn catch_up(conn: &mut Connection, progress: &mut Progress) -> Result<Step, WaitFailure> {
    while progress.phase == Phase::Replaying {
        let signal = progress.observe(&conn.recv().await?);
        progress.refusal()?;
        if signal == Signal::Closed {
            return Ok(Step::Answer(progress.ended(false)));
        }
    }
    Ok(Step::Answer(progress.absent()))
}

/// Read pushed frames until one changes the answer.
async fn await_change(
    conn: &mut Connection,
    progress: &mut Progress,
) -> Result<Signal, WaitFailure> {
    loop {
        let signal = progress.observe(&conn.recv().await?);
        progress.refusal()?;
        if signal != Signal::Nothing {
            return Ok(signal);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::frame::{Command, ControlAction, EventStamp};
    use phux_protocol::wire::info::SessionSnapshot;
    use tokio::net::UnixListener;

    use super::*;
    use crate::testkit::{EndOfScript, ScriptSpec, ScriptedServer};

    const SERVER_ID: [u8; 4] = [0xab, 0xcd, 0x01, 0x02];

    fn pane() -> ResourceId {
        ResourceId::local(7)
    }

    fn snapshot(resources: Vec<ResourceInfo>) -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
            .with_resources(resources)
    }

    fn live() -> SessionSnapshot {
        snapshot(vec![ResourceInfo::new(pane(), WindowId::new(1), 80, 24)])
    }

    fn absent() -> SessionSnapshot {
        snapshot(Vec::new())
    }

    fn retained(status: i32) -> SessionSnapshot {
        snapshot(vec![
            ResourceInfo::new(pane(), WindowId::new(1), 80, 24)
                .with_lifecycle(ResourceLifecycle::Exited)
                .with_exit(Some(
                    ExitFacet::new(1_000, 9_000).with_exit_status(Some(status)),
                )),
        ])
    }

    fn journal_spec() -> ScriptSpec {
        ScriptSpec::new()
            .server_features(ServerFeatureSet::with(&[ServerFeature::EventJournal]))
            .server_id(SERVER_ID.to_vec())
    }

    fn event_for(terminal: ResourceId, event: AgentEvent, seq: u64) -> FrameKind {
        FrameKind::Event {
            terminal: Some(terminal),
            event,
            stamp: Some(Box::new(EventStamp::new(seq, 5_000 + seq))),
        }
    }

    fn event(event: AgentEvent, seq: u64) -> FrameKind {
        event_for(pane(), event, seq)
    }

    fn closed(status: i32, seq: u64) -> FrameKind {
        event(
            AgentEvent::ResourceClosed {
                exit_status: Some(status),
            },
            seq,
        )
    }

    fn gap(first_missing: u64, last_missing: u64) -> FrameKind {
        FrameKind::Event {
            terminal: Some(pane()),
            event: AgentEvent::JournalGap {
                first_missing,
                last_missing,
            },
            stamp: None,
        }
    }

    fn cursor(seq: u64) -> Cursor {
        Cursor::new(SERVER_ID.to_vec(), seq).unwrap()
    }

    async fn run(
        spec: ScriptSpec,
        after: Option<Cursor>,
        budget: Duration,
    ) -> (Result<ResourceWait, ResourceWaitError>, Vec<FrameKind>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("wait.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
        let result =
            wait_for_exit(&socket, pane(), after.as_ref(), Deadline::new(Some(budget))).await;
        let seen = server.await.expect("scripted server");
        (result, seen)
    }

    /// Two scripted connections, one after the other: the wait reconnects.
    async fn run_twice(
        first: ScriptSpec,
        second: ScriptSpec,
        after: Option<Cursor>,
    ) -> (
        Result<ResourceWait, ResourceWaitError>,
        Vec<FrameKind>,
        Vec<FrameKind>,
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("wait.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        let server = tokio::spawn(async move {
            let one = ScriptedServer::accept(&listener, first).await;
            let two = ScriptedServer::accept(&listener, second).await;
            (one, two)
        });
        let result = wait_for_exit(
            &socket,
            pane(),
            after.as_ref(),
            Deadline::new(Some(Duration::from_secs(20))),
        )
        .await;
        let (one, two) = server.await.expect("scripted server");
        (result, one, two)
    }

    fn subscribed_after(seen: &[FrameKind]) -> Option<u64> {
        seen.iter()
            .find_map(|frame| match frame {
                FrameKind::SubscribeEvents {
                    terminal: Some(id),
                    after_seq,
                } if *id == pane() => Some(*after_seq),
                _ => None,
            })
            .expect("the wait subscribes to the resource")
    }

    fn state_reads(seen: &[FrameKind]) -> usize {
        seen.iter()
            .filter(|frame| {
                matches!(
                    frame,
                    FrameKind::Command {
                        command: Command::GetState { .. },
                        ..
                    }
                )
            })
            .count()
    }

    #[tokio::test]
    async fn resource_wait_returns_exited_when_the_pane_already_has_an_exit_facet() {
        let spec = journal_spec().state(retained(42).with_journal_head(Some(30)));
        let (result, seen) = run(spec, None, Duration::from_secs(20)).await;
        let wait = result.expect("wait");
        assert_eq!(wait.outcome, WaitOutcome::Exited);
        assert!(wait.retained);
        let exit = wait.exit.expect("exit");
        assert_eq!(exit.status, Some(42));
        assert_eq!(exit.reason, Some("exited"));
        assert_eq!(exit.exited_at_ms, Some(1_000));
        // Subscribe strictly before the read, on the one connection.
        let subscribe = seen
            .iter()
            .position(|frame| matches!(frame, FrameKind::SubscribeEvents { .. }))
            .expect("subscribe");
        let read = seen
            .iter()
            .position(|frame| matches!(frame, FrameKind::Command { .. }))
            .expect("read");
        assert!(subscribe < read, "subscribe-before-snapshot: {seen:?}");
        assert_eq!(
            subscribed_after(&seen),
            Some(NO_REPLAY),
            "no cursor: journal semantics, no replay"
        );
        assert_eq!(
            wait.cursor.expect("cursor").to_string(),
            "abcd0102:30",
            "a wait that saw no event resumes from the head, not from 0"
        );
    }

    #[tokio::test]
    async fn resource_wait_resumes_from_a_cursor_and_replays_the_close() {
        let spec = journal_spec()
            .push(closed(3, 41))
            .state(absent().with_journal_head(Some(41)));
        let (result, seen) = run(spec, Some(cursor(40)), Duration::from_secs(20)).await;
        let wait = result.expect("wait");
        assert_eq!(subscribed_after(&seen), Some(40));
        assert_eq!(
            wait.outcome,
            WaitOutcome::Exited,
            "the replayed close beats `gone`"
        );
        assert!(!wait.retained);
        let exit = wait.exit.expect("exit");
        assert_eq!(exit.status, Some(3));
        assert_eq!(exit.exited_at_ms, Some(5_041));
        assert_eq!(wait.cursor.expect("cursor").seq(), 41);
        assert!(!wait.cursor_void);
    }

    /// The review's reproduction: more replayed events than the connection's
    /// mailbox takes, so the state answer overtakes the replay and the close
    /// arrives after it. The wait keeps reading through the head.
    #[tokio::test]
    async fn a_state_answer_that_overtakes_the_replay_waits_for_the_head() {
        let bells: Vec<FrameKind> = (41..=45).map(|seq| event(AgentEvent::Bell, seq)).collect();
        let mut rest: Vec<FrameKind> = (46..=48).map(|seq| event(AgentEvent::Bell, seq)).collect();
        rest.push(closed(5, 49));
        let spec = journal_spec()
            .extend(bells)
            .state(absent().with_journal_head(Some(49)))
            .push_after_state(rest);
        let (result, seen) = run(spec, Some(cursor(40)), Duration::from_secs(20)).await;
        let wait = result.expect("wait");
        assert_eq!(state_reads(&seen), 1);
        assert_eq!(wait.outcome, WaitOutcome::Exited, "not gone: {wait:?}");
        assert_eq!(wait.exit.expect("exit").status, Some(5));
        assert_eq!(wait.cursor.expect("cursor").seq(), 49);
    }

    /// Once the replay reaches the head with no close in it, the absence is
    /// honest.
    #[tokio::test]
    async fn a_replay_that_reaches_the_head_without_a_close_is_gone() {
        let spec = journal_spec()
            .push(event(AgentEvent::Bell, 41))
            .state(absent().with_journal_head(Some(43)))
            .push_after_state(vec![
                event_for(ResourceId::local(99), AgentEvent::Bell, 42),
                event(AgentEvent::Bell, 43),
            ]);
        let (result, _seen) = run(spec, Some(cursor(40)), Duration::from_secs(20)).await;
        let wait = result.expect("wait");
        assert_eq!(wait.outcome, WaitOutcome::Gone);
        assert_eq!(wait.cursor.expect("cursor").seq(), 43);
    }

    /// A gap during the replay is the journal's eviction: the range is
    /// covered, not a reason to reconnect.
    #[tokio::test]
    async fn an_eviction_gap_during_the_replay_covers_its_range() {
        let spec = journal_spec()
            .state(absent().with_journal_head(Some(50)))
            .push_after_state(vec![gap(41, 50)]);
        let (result, seen) = run(spec, Some(cursor(40)), Duration::from_secs(20)).await;
        let wait = result.expect("wait");
        assert_eq!(state_reads(&seen), 1, "no reconnect for an eviction gap");
        assert_eq!(wait.outcome, WaitOutcome::Gone);
        assert!(
            wait.evidence_lost,
            "the evicted range may have held the close"
        );
        assert_eq!(wait.to_json()["evidence_lost"], true);
        assert_eq!(wait.cursor.expect("cursor").seq(), 50);
    }

    /// The head is per connection (L1 §7.3): events on other terminals do
    /// not raise it, so a head at or below the cursor is caught up and a
    /// closed pane's absence answers `gone` at once, not at the deadline.
    #[tokio::test]
    async fn a_head_owned_by_another_terminal_is_gone_not_a_timeout() {
        let started = std::time::Instant::now();
        let spec = journal_spec().state(absent().with_journal_head(Some(35)));
        let (result, seen) = run(spec, Some(cursor(40)), Duration::from_secs(20)).await;
        let wait = result.expect("wait");
        assert_eq!(subscribed_after(&seen), Some(40));
        assert_eq!(wait.outcome, WaitOutcome::Gone);
        assert!(!wait.evidence_lost);
        assert_eq!(wait.cursor.expect("cursor").seq(), 40, "never moves back");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "no deadline wait"
        );
    }

    /// A gap past the head is live even while the client is still taking
    /// the replay: it reconnects and recovers the close the gap hid.
    #[tokio::test]
    async fn a_gap_past_the_head_reconnects_and_recovers_the_close() {
        let mut replay: Vec<FrameKind> =
            (41..=50).map(|seq| event(AgentEvent::Bell, seq)).collect();
        replay.push(gap(51, 60));
        let first = journal_spec()
            .extend(replay)
            .state(live().with_journal_head(Some(50)));
        let second = journal_spec()
            .push(closed(4, 55))
            .state(absent().with_journal_head(Some(55)));
        let (result, one, two) = run_twice(first, second, Some(cursor(40))).await;
        let wait = result.expect("wait");
        assert_eq!(subscribed_after(&one), Some(40));
        assert_eq!(
            subscribed_after(&two),
            Some(50),
            "resumes from the last seq it accounted for"
        );
        assert_eq!(wait.outcome, WaitOutcome::Exited, "not swallowed: {wait:?}");
        assert_eq!(wait.exit.expect("exit").status, Some(4));
        assert!(!wait.evidence_lost);
    }

    /// A gap past the head that every connection repeats (the loop a
    /// satellite cursor once caused behind a hub) ends at the deadline, and
    /// the reconnects back off instead of spinning.
    #[tokio::test]
    async fn a_gap_past_the_head_on_every_connection_ends_at_the_deadline() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("wait.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        let connections = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&connections);
        let server = tokio::spawn(async move {
            loop {
                let spec = journal_spec()
                    .state(live().with_journal_head(Some(10)))
                    .push_after_state(vec![gap(11, 12)]);
                let _ = ScriptedServer::accept(&listener, spec).await;
                counted.fetch_add(1, Ordering::SeqCst);
            }
        });
        let wait = wait_for_exit(
            &socket,
            pane(),
            Some(&cursor(10)),
            Deadline::new(Some(Duration::from_millis(600))),
        )
        .await
        .expect("wait");
        server.abort();
        assert_eq!(wait.outcome, WaitOutcome::TimedOut);
        assert_eq!(wait.cursor.expect("cursor").seq(), 10);
        let made = connections.load(Ordering::SeqCst);
        assert!((1..10).contains(&made), "backs off: {made} connections");
    }

    /// A scoped workload whose state read is refused is denied (exit 2 at
    /// the CLI), not a missing server.
    #[tokio::test]
    async fn a_refused_state_read_is_denied() {
        let spec = journal_spec().refuse_state(ErrorCode::PermissionDenied, "permission denied");
        let (result, _seen) = run(spec, None, Duration::from_secs(20)).await;
        let err = result.expect_err("denied");
        assert!(
            matches!(&err.cause, WaitFailure::Denied(message) if message == "permission denied"),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn a_cursor_from_another_incarnation_is_void_and_falls_back_to_the_level_read() {
        let foreign = Cursor::new(vec![0x99; 4], 40).unwrap();
        let (result, seen) = run(
            journal_spec().state(retained(0).with_journal_head(Some(12))),
            Some(foreign),
            Duration::from_secs(20),
        )
        .await;
        let wait = result.expect("wait");
        assert_eq!(
            subscribed_after(&seen),
            Some(NO_REPLAY),
            "a void cursor is not sent"
        );
        assert!(wait.cursor_void);
        assert_eq!(wait.outcome, WaitOutcome::Exited);
        let fresh = wait.cursor.expect("fresh cursor");
        assert_eq!(fresh.server_id(), SERVER_ID);
        assert_eq!(fresh.seq(), 12);
    }

    /// A live `journal_gap` (a mailbox overflow) may hide a close the ring
    /// still holds, and the subscription never re-delivers it: the wait
    /// resumes on a fresh connection from the last `seq` it accounted for.
    #[tokio::test]
    async fn a_live_gap_resumes_on_a_fresh_connection_from_the_last_seq_seen() {
        let first = journal_spec()
            .states([live().with_journal_head(Some(10)), absent()])
            .push_after_state(vec![gap(11, 12)]);
        let second = journal_spec()
            .push(closed(4, 11))
            .state(absent().with_journal_head(Some(12)));
        let (result, one, two) = run_twice(first, second, None).await;
        let wait = result.expect("wait");
        assert_eq!(subscribed_after(&one), Some(NO_REPLAY));
        assert_eq!(
            subscribed_after(&two),
            Some(10),
            "the second connection resumes from the cut"
        );
        assert_eq!(
            wait.outcome,
            WaitOutcome::Exited,
            "the recovered close, not `gone`"
        );
        assert_eq!(wait.exit.expect("exit").status, Some(4));
    }

    #[tokio::test]
    async fn a_live_exit_event_rereads_for_the_retained_facet() {
        let exited = event(
            AgentEvent::TerminalControl {
                lifecycle: ResourceLifecycle::Exited,
                exit_status: Some(9),
                input_holder: None,
                action: ControlAction::Exited,
                actor: None,
            },
            12,
        );
        let spec = journal_spec()
            .states([live(), retained(9)])
            .push_after_state(vec![exited]);
        let (result, seen) = run(spec, None, Duration::from_secs(20)).await;
        let wait = result.expect("wait");
        assert_eq!(state_reads(&seen), 2);
        assert!(wait.retained);
        assert_eq!(wait.exit.expect("exit").exited_at_ms, Some(1_000));
        assert_eq!(wait.cursor.expect("cursor").seq(), 12);
    }

    #[tokio::test]
    async fn a_live_close_answers_without_another_read() {
        let spec = journal_spec()
            .state(live())
            .push_after_state(vec![closed(1, 20)]);
        let (result, seen) = run(spec, None, Duration::from_secs(20)).await;
        let wait = result.expect("wait");
        assert_eq!(state_reads(&seen), 1);
        assert_eq!(wait.outcome, WaitOutcome::Exited);
        assert!(!wait.retained);
        assert_eq!(wait.exit.expect("exit").status, Some(1));
    }

    #[tokio::test]
    async fn resource_wait_reports_gone_for_an_unknown_id() {
        let (result, _seen) = run(
            journal_spec().state(absent()),
            None,
            Duration::from_secs(20),
        )
        .await;
        let wait = result.expect("wait");
        assert_eq!(wait.outcome, WaitOutcome::Gone);
        assert!(wait.exit.is_none());
        assert_eq!(wait.to_json()["outcome"], "gone");
    }

    #[tokio::test]
    async fn an_absence_in_a_partial_view_is_not_gone() {
        let spec = journal_spec()
            .degradation_notice("satellite edge is unreachable")
            .state(absent());
        let (result, _seen) = run(spec, None, Duration::from_secs(20)).await;
        let err = result.expect_err("partial view");
        assert!(
            matches!(&err.cause, WaitFailure::PartialView(notices) if notices.len() == 1),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn the_deadline_times_out_a_live_pane_and_keeps_the_cursor() {
        let (result, _seen) = run(
            journal_spec().state(live()),
            Some(cursor(17)),
            Duration::from_millis(150),
        )
        .await;
        let wait = result.expect("wait");
        assert_eq!(wait.outcome, WaitOutcome::TimedOut);
        assert!(wait.exit.is_none());
        assert_eq!(wait.cursor.as_ref().expect("cursor").seq(), 17);
        let doc = wait.to_json();
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["resource"], "@7");
        assert_eq!(doc["cursor"], "abcd0102:17");
        assert!(doc["exit"].is_null());
    }

    /// An exit or close the wait saw before its deadline answers `exited`
    /// even when the confirming read never came back.
    #[test]
    fn a_timeout_after_an_observed_exit_reports_exited() {
        let mut progress = Progress::new(pane(), None);
        progress.observe(&event(
            AgentEvent::TerminalControl {
                lifecycle: ResourceLifecycle::Exited,
                exit_status: Some(2),
                input_holder: None,
                action: ControlAction::Exited,
                actor: None,
            },
            8,
        ));
        let wait = progress.finish(None, Duration::from_secs(1));
        assert_eq!(wait.outcome, WaitOutcome::Exited);
        assert!(wait.retained, "an exit notice without a close is retained");
        assert_eq!(wait.exit.expect("exit").status, Some(2));

        let mut closed_first = Progress::new(pane(), None);
        closed_first.observe(&closed(6, 9));
        let wait = closed_first.finish(None, Duration::from_secs(1));
        assert_eq!(wait.outcome, WaitOutcome::Exited);
        assert!(!wait.retained);
    }

    /// A wait cut short by an error still names where it got to.
    #[tokio::test]
    async fn an_error_carries_the_cursor_it_reached() {
        let spec = journal_spec().end(EndOfScript::HangUp);
        let (result, _seen) = run(spec, Some(cursor(5)), Duration::from_secs(20)).await;
        let err = result.expect_err("the server hung up before the answer");
        assert!(matches!(err.cause, WaitFailure::Attach(_)), "{err:?}");
        assert_eq!(err.cursor.expect("cursor").seq(), 5);
    }

    #[tokio::test]
    async fn a_server_without_a_journal_issues_no_cursor() {
        let (result, seen) = run(
            ScriptSpec::new().state(retained(0)),
            None,
            Duration::from_secs(20),
        )
        .await;
        let wait = result.expect("wait");
        assert_eq!(subscribed_after(&seen), None, "live only: no journal");
        assert!(wait.cursor.is_none());
        assert!(wait.to_json()["cursor"].is_null());
    }

    /// A scoped server refuses an unobservable subscription with an
    /// uncorrelated `PERMISSION_DENIED` and nothing else (workload-auth §7);
    /// the wait ends on it at once instead of waiting out its deadline for
    /// events that will never come.
    #[tokio::test]
    async fn a_refused_subscription_ends_the_wait_promptly() {
        let refusal = FrameKind::Error {
            request_id: None,
            code: ErrorCode::PermissionDenied,
            message: "permission denied".to_owned(),
        };
        let started = std::time::Instant::now();
        let (result, _seen) = run(
            journal_spec().push(refusal).state(live()),
            None,
            Duration::from_secs(20),
        )
        .await;
        let err = result.expect_err("denied");
        assert!(
            matches!(&err.cause, WaitFailure::Denied(message) if message == "permission denied"),
            "{err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "no deadline wait"
        );
    }
}
