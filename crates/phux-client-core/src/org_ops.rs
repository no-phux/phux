//! Transport-free organization operations: the retry journal and the
//! kill-session planner.
//!
//! Every mutating organization command carries a caller-minted
//! [`OperationId`]. The server keeps a receipt per id, so a resend with the
//! same id and payload returns the original result without repeating its
//! effects, and the same id with another payload is refused
//! `DUPLICATE_OPERATION` (`docs/spec/L1.md` §5.7.2). [`OrgJournal`] is the
//! client half of that contract, on the [`InputReplayJournal`] pattern:
//!
//! - **One id per user action, forever.** A resend after a reconnect reuses
//!   the id and the payload verbatim; [`OrgJournal::submit`] refuses an id
//!   that is already journaled with another payload.
//! - **Resend only inside the horizon and to the same authority.** Receipts
//!   live in the authority's organization store, so they survive a server
//!   restart but not a different authority, and the server prunes them. Past
//!   [`OrgJournal::horizon_ms`] an attempted operation is never resent: it
//!   resolves [`OrgOutcome::Unknown`] and the caller reconciles from a fresh
//!   `GET_ORGANIZATION`.
//! - **Never-sent is refused, attempted is unknown.** An operation that never
//!   reached a socket and can no longer be sent is
//!   [`OrgOutcome::NotSent`]; one that might have reached the server is
//!   [`OrgOutcome::Unknown`].
//!
//! [`KillWorkflow`] plans `phux kill <session>`, which is a client workflow
//! rather than one command (CONTRACTS §04): terminate every executing
//! member, wait until each is inactive, remove each, then delete the session
//! with the revision the workflow last committed against. The member set is
//! fixed when the workflow starts; work added meanwhile survives and makes
//! the final delete refuse `NOT_EMPTY`.
//!
//! [`InputReplayJournal`]: crate::input_replay::InputReplayJournal

use std::collections::{HashMap, VecDeque};

use phux_protocol::ids::{AuthorityId, OperationId, ResourceKey, Revision, SessionKey};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, Committed, ErrorCode, ExecState, FrameKind, LiveTarget,
    Refusal, ResourceRecord, SessionRecord, TerminateMode,
};
use sha2::{Digest, Sha256};

/// Default horizon for resending an attempted operation.
///
/// Well inside the server's receipt retention (24 hours or 10,000 receipts,
/// whichever comes first), because a busy authority prunes by count long
/// before a day.
pub const ORG_RETRY_HORIZON_MS: u64 = 10 * 60 * 1_000;

/// Most operations one journal retains.
pub const ORG_JOURNAL_MAX_OPS: usize = 1024;

/// How an organization operation ended, from the caller's point of view.
#[derive(Debug, Clone, PartialEq)]
pub enum OrgOutcome {
    /// The edit committed; the receipt is what an undo needs.
    Committed(Box<Committed>),
    /// The command was accepted; its effects follow as events.
    Accepted,
    /// A read answered with a value (`GET_ORGANIZATION`, `ARRANGEMENT_GET`).
    Value(Box<CommandValue>),
    /// The server refused the command; nothing changed.
    Refused(Refusal),
    /// The server answered with a legacy `ERROR`.
    Error {
        /// The wire error code.
        code: ErrorCode,
        /// The server's message.
        message: String,
    },
    /// The command might have reached the server and no resend can say
    /// whether it committed. Reconcile from `GET_ORGANIZATION`.
    Unknown {
        /// Why the outcome is unknown.
        reason: String,
    },
    /// The command never reached a socket and can no longer be sent.
    NotSent {
        /// Why it was not sent.
        reason: String,
    },
}

impl OrgOutcome {
    /// Classify an organization command's reply.
    #[must_use]
    pub fn from_result(result: &CommandResult) -> Self {
        match result {
            CommandResult::OkWith(CommandValue::Committed(committed)) => {
                Self::Committed(committed.clone())
            }
            CommandResult::OkWith(value) => Self::Value(Box::new(value.clone())),
            CommandResult::Accepted { .. } | CommandResult::Ok => Self::Accepted,
            CommandResult::Refused(refusal) => Self::Refused(refusal.clone()),
            CommandResult::Error { code, message } => Self::Error {
                code: *code,
                message: message.clone(),
            },
            // `CommandResult` is non-exhaustive: a reply this build cannot
            // read is not evidence that nothing changed.
            _ => Self::Unknown {
                reason: "the server answered with a result this build cannot read".to_owned(),
            },
        }
    }

    /// Whether the operation is known to have taken effect.
    #[must_use]
    pub const fn succeeded(&self) -> bool {
        matches!(self, Self::Committed(_) | Self::Accepted | Self::Value(_))
    }
}

/// One resolved journal operation.
#[derive(Debug, Clone, PartialEq)]
pub struct OrgReport {
    /// The operation.
    pub operation: OperationId,
    /// How it ended.
    pub outcome: OrgOutcome,
}

/// Why [`OrgJournal::submit`] refused an operation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubmitError {
    /// The command is not a mutating organization command (it carries no
    /// operation id).
    #[error("not a mutating organization command")]
    NotAnOperation,
    /// The id is journaled with another payload; the server would refuse it
    /// `DUPLICATE_OPERATION`.
    #[error("operation id already used with a different payload")]
    DuplicateOperation,
    /// The journal is full.
    #[error("too many organization operations are pending")]
    Full,
}

#[derive(Debug)]
struct JournalOp {
    operation: OperationId,
    command: Command,
    payload: [u8; 32],
    submitted_ms: u64,
    /// The authority the first attempt was sent to; `None` until known.
    authority: Option<AuthorityId>,
    attempts: u32,
    in_flight: Option<u32>,
}

/// The organization retry journal. See the module docs.
#[derive(Debug)]
pub struct OrgJournal {
    ops: VecDeque<JournalOp>,
    horizon_ms: u64,
    /// The authority of the live connection, once known.
    authority: Option<AuthorityId>,
    /// Whether a live connection advertises `ORGANIZATION`.
    live: bool,
}

impl Default for OrgJournal {
    fn default() -> Self {
        Self::new(ORG_RETRY_HORIZON_MS)
    }
}

impl OrgJournal {
    /// An empty journal resending attempted operations for `horizon_ms`.
    #[must_use]
    pub const fn new(horizon_ms: u64) -> Self {
        Self {
            ops: VecDeque::new(),
            horizon_ms,
            authority: None,
            live: false,
        }
    }

    /// The resend horizon, in milliseconds.
    #[must_use]
    pub const fn horizon_ms(&self) -> u64 {
        self.horizon_ms
    }

    /// How many operations are unresolved.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Whether nothing is unresolved.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// The unresolved operations' ids, in submission order.
    pub fn pending(&self) -> impl Iterator<Item = OperationId> + '_ {
        self.ops.iter().map(|op| op.operation)
    }

    /// The command journaled under `operation`.
    #[must_use]
    pub fn command(&self, operation: &OperationId) -> Option<&Command> {
        self.ops
            .iter()
            .find(|op| op.operation == *operation)
            .map(|op| &op.command)
    }

    /// Journal `command` for sending. A resubmit of the same id with the
    /// same payload is accepted and journaled once.
    ///
    /// # Errors
    ///
    /// [`SubmitError`] — see its variants.
    pub fn submit(&mut self, command: Command, now_ms: u64) -> Result<OperationId, SubmitError> {
        let operation = *command
            .organization_operation()
            .ok_or(SubmitError::NotAnOperation)?;
        let payload = payload_hash(&command);
        if let Some(existing) = self.ops.iter().find(|op| op.operation == operation) {
            return if existing.payload == payload {
                Ok(operation)
            } else {
                Err(SubmitError::DuplicateOperation)
            };
        }
        if self.ops.len() >= ORG_JOURNAL_MAX_OPS {
            return Err(SubmitError::Full);
        }
        self.ops.push_back(JournalOp {
            operation,
            command,
            payload,
            submitted_ms: now_ms,
            authority: None,
            attempts: 0,
            in_flight: None,
        });
        Ok(operation)
    }

    /// A connection negotiated. `organization` is whether it advertises
    /// `ORGANIZATION`. Without it every operation resolves now: never-sent
    /// ones [`OrgOutcome::NotSent`], attempted ones [`OrgOutcome::Unknown`].
    /// Attempted operations past the horizon resolve unknown too.
    pub fn begin_connection(&mut self, organization: bool, now_ms: u64) -> Vec<OrgReport> {
        self.clear_in_flight();
        self.live = organization;
        self.authority = None;
        if !organization {
            return self.strand_all("the server does not advertise ORGANIZATION");
        }
        self.expire(now_ms)
    }

    /// The live connection's authority, learned from a `GET_ORGANIZATION`
    /// page. Attempted operations sent to another authority resolve unknown:
    /// that authority's receipts are not here.
    pub fn set_authority(&mut self, authority: AuthorityId) -> Vec<OrgReport> {
        self.authority = Some(authority);
        let mut reports = Vec::new();
        let mut kept = VecDeque::with_capacity(self.ops.len());
        for op in self.ops.drain(..) {
            match op.authority {
                Some(sent_to) if sent_to != authority => reports.push(OrgReport {
                    operation: op.operation,
                    outcome: OrgOutcome::Unknown {
                        reason: "the server now serves a different authority".to_owned(),
                    },
                }),
                _ => kept.push_back(op),
            }
        }
        self.ops = kept;
        reports
    }

    /// The transport dropped: nothing is in flight any more, and nothing is
    /// sent until [`Self::begin_connection`].
    pub fn connection_lost(&mut self) {
        self.clear_in_flight();
        self.live = false;
    }

    /// Resolve attempted operations past the horizon as unknown.
    pub fn expire(&mut self, now_ms: u64) -> Vec<OrgReport> {
        let horizon = self.horizon_ms;
        let mut reports = Vec::new();
        let mut kept = VecDeque::with_capacity(self.ops.len());
        for op in self.ops.drain(..) {
            let expired = op.attempts > 0
                && op.in_flight.is_none()
                && now_ms.saturating_sub(op.submitted_ms) > horizon;
            if expired {
                reports.push(OrgReport {
                    operation: op.operation,
                    outcome: OrgOutcome::Unknown {
                        reason: "the retry horizon passed before the server answered".to_owned(),
                    },
                });
            } else {
                kept.push_back(op);
            }
        }
        self.ops = kept;
        reports
    }

    /// Frames for every operation not in flight, each under a request id
    /// from `request_id`, in submission order. Nothing while no live
    /// connection advertises `ORGANIZATION`.
    pub fn next_frames(&mut self, request_id: &mut dyn FnMut() -> u32) -> Vec<FrameKind> {
        if !self.live {
            return Vec::new();
        }
        let authority = self.authority;
        let mut frames = Vec::new();
        for op in self.ops.iter_mut().filter(|op| op.in_flight.is_none()) {
            let id = request_id();
            op.in_flight = Some(id);
            op.attempts = op.attempts.saturating_add(1);
            if op.authority.is_none() {
                op.authority = authority;
            }
            frames.push(FrameKind::Command {
                request_id: id,
                command: op.command.clone(),
            });
        }
        frames
    }

    /// Whether `request_id` is an attempt this journal sent.
    #[must_use]
    pub fn owns(&self, request_id: u32) -> bool {
        self.ops.iter().any(|op| op.in_flight == Some(request_id))
    }

    /// Resolve the attempt `request_id` with its reply. `None` when the
    /// journal did not send it.
    pub fn resolve(&mut self, request_id: u32, result: &CommandResult) -> Option<OrgReport> {
        let position = self
            .ops
            .iter()
            .position(|op| op.in_flight == Some(request_id))?;
        let op = self.ops.remove(position)?;
        Some(OrgReport {
            operation: op.operation,
            outcome: OrgOutcome::from_result(result),
        })
    }

    /// Resolve everything still journaled: the no-more-reconnects teardown.
    pub fn drain(&mut self, why: &str) -> Vec<OrgReport> {
        self.strand_all(why)
    }

    fn clear_in_flight(&mut self) {
        for op in &mut self.ops {
            op.in_flight = None;
        }
    }

    fn strand_all(&mut self, why: &str) -> Vec<OrgReport> {
        self.ops
            .drain(..)
            .map(|op| OrgReport {
                operation: op.operation,
                outcome: if op.attempts == 0 {
                    OrgOutcome::NotSent {
                        reason: why.to_owned(),
                    }
                } else {
                    OrgOutcome::Unknown {
                        reason: why.to_owned(),
                    }
                },
            })
            .collect()
    }
}

/// A digest of `command`'s payload, operation id included: what the server
/// compares a same-id resend against.
#[must_use]
pub fn payload_hash(command: &Command) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(format!("{command:?}").as_bytes());
    if let Some(operation) = command.organization_operation() {
        hash.update(operation.as_bytes());
    }
    hash.finalize().into()
}

/// Extension for the protocol's command type.
pub trait OrganizationCommand {
    /// The operation id of a mutating organization command.
    fn organization_operation(&self) -> Option<&OperationId>;
}

impl OrganizationCommand for Command {
    fn organization_operation(&self) -> Option<&OperationId> {
        if self.is_organization() {
            self.idempotency_key()
        } else {
            None
        }
    }
}

/// Whether an execution state is finished: nothing runs, and the record may
/// be removed or relaunched.
#[must_use]
pub const fn is_inactive(state: Option<ExecState>) -> bool {
    matches!(
        state,
        None | Some(ExecState::Completed | ExecState::Failed | ExecState::Abandoned)
    )
}

/// The live target that addresses `record`'s current activation, when it
/// executes on this server.
#[must_use]
pub fn live_target(record: &ResourceRecord) -> Option<LiveTarget> {
    let activation = record.activation.as_ref()?;
    Some(LiveTarget {
        resource: record.live.clone()?,
        instance: activation.instance,
        activation: activation.id,
    })
}

/// A sentence for a refusal, as a frontend shows it.
#[must_use]
pub fn refusal_sentence(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Unauthorized { verb } => format!("not authorized ({verb:?} is required)"),
        Refusal::NotFound => "no such record".to_owned(),
        Refusal::Conflict { current } => {
            format!("it changed meanwhile (now at revision {current})")
        }
        Refusal::Stale { .. } => "that execution is no longer current".to_owned(),
        Refusal::NotEmpty { members } => format!("the session still has {members} member(s)"),
        Refusal::Unsupported { capability } => format!("the server does not support {capability}"),
        Refusal::InvalidArgument { field, reason } => format!("invalid {field}: {reason}"),
        Refusal::NameTaken => "that name is taken".to_owned(),
        Refusal::ExecutionUncertain => {
            "a lost execution may still be running; resolve it first".to_owned()
        }
        Refusal::LeaseHeld { .. } => "another principal holds the input lease".to_owned(),
        Refusal::DuplicateOperation => {
            "the operation id was already used for a different request".to_owned()
        }
        Refusal::StoreFailure { reason } => {
            format!("the server could not save the change ({reason}); retry")
        }
        other => format!("refused ({})", other.as_str()),
    }
}

// ---------------------------------------------------------------------------
// The kill-session workflow.
// ---------------------------------------------------------------------------

/// How many times one remove or the final delete is retried after a
/// revision conflict.
const MAX_CONFLICT_RETRIES: u32 = 3;

/// How one member of a killed session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetOutcome {
    /// The workflow removed it.
    Removed,
    /// It was gone before the workflow removed it (it exited and its exit
    /// policy removed it, or another client did).
    AlreadyGone,
    /// The server refused a step.
    Refused(Refusal),
    /// A step's outcome is unknown.
    Unknown(String),
    /// It did not become inactive in time.
    TimedOut,
    /// It is executing but not on this server, so it cannot be terminated
    /// from here.
    NoLiveTarget,
}

impl TargetOutcome {
    /// Whether the member no longer exists.
    #[must_use]
    pub const fn is_gone(&self) -> bool {
        matches!(self, Self::Removed | Self::AlreadyGone)
    }
}

/// How the session itself ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOutcome {
    /// The workflow deleted it.
    Deleted,
    /// It was gone already (an ephemeral session deletes itself with its
    /// last member).
    AlreadyGone,
    /// A member could not be removed, so the delete was not sent.
    Kept,
    /// The server refused the delete (`NotEmpty` when work arrived meanwhile).
    Refused(Refusal),
    /// The delete's outcome is unknown.
    Unknown(String),
}

/// The per-target result of a kill workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillReport {
    /// The session.
    pub session: SessionKey,
    /// Each member of the fixed target set, in membership order.
    pub targets: Vec<(ResourceKey, TargetOutcome)>,
    /// The session's own outcome.
    pub session_outcome: SessionOutcome,
}

impl KillReport {
    /// Whether the whole session went away.
    #[must_use]
    pub const fn complete(&self) -> bool {
        matches!(
            self.session_outcome,
            SessionOutcome::Deleted | SessionOutcome::AlreadyGone
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Step {
    Terminate(LiveTarget),
    Terminating,
    AwaitInactive,
    Remove { expected: Revision, retries: u32 },
    Removing { retries: u32 },
    Done(TargetOutcome),
}

#[derive(Debug, Clone)]
struct Member {
    key: ResourceKey,
    step: Step,
}

#[derive(Debug, Clone, PartialEq)]
enum DeleteStep {
    Waiting,
    Ready { expected: Revision, retries: u32 },
    Deleting { retries: u32 },
    Done(SessionOutcome),
}

#[derive(Debug, Clone, Copy)]
enum Issued {
    Terminate(ResourceKey),
    Remove(ResourceKey),
    Delete,
}

/// The pure kill-session planner. See the module docs.
///
/// Drive it by sending [`Self::next_commands`], feeding replies to
/// [`Self::on_result`], execution changes to [`Self::on_activation`] (from
/// `ACTIVATION_CHANGED` events or a refreshed record via
/// [`Self::on_records`]), and removals to [`Self::on_removed`], until
/// [`Self::is_done`]; then read [`Self::report`].
#[derive(Debug, Clone)]
pub struct KillWorkflow {
    session: SessionKey,
    session_revision: Revision,
    members: Vec<Member>,
    delete: DeleteStep,
    issued: HashMap<OperationId, Issued>,
    mode: TerminateMode,
}

impl KillWorkflow {
    /// Plan the kill of `session`, whose members are the `resources` whose
    /// `session` field names it (others are ignored).
    #[must_use]
    pub fn plan(session: &SessionRecord, resources: &[ResourceRecord]) -> Self {
        let members = resources
            .iter()
            .filter(|record| record.session == session.key)
            .map(|record| Member {
                key: record.key,
                step: initial_step(record),
            })
            .collect();
        let mut workflow = Self {
            session: session.key,
            session_revision: session.revision,
            members,
            delete: DeleteStep::Waiting,
            issued: HashMap::new(),
            mode: TerminateMode::Graceful,
        };
        workflow.advance_delete();
        workflow
    }

    /// The session being killed.
    #[must_use]
    pub const fn session(&self) -> SessionKey {
        self.session
    }

    /// The fixed target set.
    pub fn targets(&self) -> impl Iterator<Item = ResourceKey> + '_ {
        self.members.iter().map(|member| member.key)
    }

    /// Members waiting to become inactive.
    pub fn awaiting(&self) -> impl Iterator<Item = ResourceKey> + '_ {
        self.members
            .iter()
            .filter(|member| matches!(member.step, Step::AwaitInactive | Step::Terminating))
            .map(|member| member.key)
    }

    /// Commands to send now, each under an operation id from `mint`.
    pub fn next_commands(
        &mut self,
        mint: &mut dyn FnMut() -> OperationId,
    ) -> Vec<(OperationId, Command)> {
        let mut out = Vec::new();
        for member in &mut self.members {
            let command = match &member.step {
                Step::Terminate(target) => {
                    let operation = mint();
                    let command = Command::ResourceTerminate {
                        operation,
                        target: target.clone(),
                        mode: self.mode,
                    };
                    member.step = Step::Terminating;
                    self.issued.insert(operation, Issued::Terminate(member.key));
                    Some((operation, command))
                }
                Step::Remove { retries, .. } => {
                    let operation = mint();
                    let command = Command::ResourceRemove {
                        operation,
                        resource: member.key,
                    };
                    member.step = Step::Removing { retries: *retries };
                    self.issued.insert(operation, Issued::Remove(member.key));
                    Some((operation, command))
                }
                _ => None,
            };
            out.extend(command);
        }
        if let DeleteStep::Ready { retries, .. } = self.delete {
            let operation = mint();
            out.push((
                operation,
                Command::SessionDelete {
                    operation,
                    session: self.session,
                },
            ));
            self.delete = DeleteStep::Deleting { retries };
            self.issued.insert(operation, Issued::Delete);
        }
        out
    }

    /// Feed the reply to a command [`Self::next_commands`] issued.
    pub fn on_result(&mut self, operation: &OperationId, outcome: &OrgOutcome) {
        let Some(issued) = self.issued.remove(operation) else {
            return;
        };
        if let OrgOutcome::Committed(committed) = outcome {
            self.track_session_revision(committed);
        }
        match issued {
            Issued::Terminate(key) => self.terminate_result(key, outcome),
            Issued::Remove(key) => self.remove_result(key, outcome),
            Issued::Delete => self.delete_result(outcome),
        }
        self.advance_delete();
    }

    /// A member's execution state changed.
    pub fn on_activation(&mut self, resource: &ResourceKey, state: ExecState) {
        if !is_inactive(Some(state)) {
            return;
        }
        if let Some(member) = self.member_mut(resource)
            && matches!(member.step, Step::AwaitInactive | Step::Terminating)
        {
            // The remove learns the current revision from a conflict.
            member.step = Step::Remove {
                expected: Revision::new(0),
                retries: 0,
            };
        }
    }

    /// A member was removed by someone (its exit policy, or another client).
    pub fn on_removed(&mut self, resource: &ResourceKey) {
        if let Some(member) = self.member_mut(resource)
            && !matches!(member.step, Step::Done(_))
        {
            member.step = Step::Done(TargetOutcome::AlreadyGone);
        }
        self.advance_delete();
    }

    /// A refreshed view of the session's records: a member missing from it
    /// is gone, an inactive one is ready to remove at its current revision.
    pub fn on_records(&mut self, session: Option<&SessionRecord>, resources: &[ResourceRecord]) {
        if let Some(session) = session.filter(|s| s.key == self.session) {
            self.session_revision = self.session_revision.max(session.revision);
        }
        for member in &mut self.members {
            if matches!(member.step, Step::Done(_)) {
                continue;
            }
            match resources.iter().find(|r| r.key == member.key) {
                None => member.step = Step::Done(TargetOutcome::AlreadyGone),
                Some(record) => {
                    let inactive = is_inactive(record.activation.map(|a| a.state));
                    if inactive && matches!(member.step, Step::AwaitInactive | Step::Terminating) {
                        member.step = Step::Remove {
                            expected: record.revision,
                            retries: 0,
                        };
                    }
                }
            }
        }
        if session.is_none() && self.members.iter().all(|m| matches!(m.step, Step::Done(_))) {
            self.delete = DeleteStep::Done(SessionOutcome::AlreadyGone);
        }
        self.advance_delete();
    }

    /// Terminate every member still waiting again, forcefully.
    pub fn escalate(&mut self, resources: &[ResourceRecord]) {
        self.mode = TerminateMode::Force;
        for member in &mut self.members {
            if matches!(member.step, Step::AwaitInactive)
                && let Some(target) = resources
                    .iter()
                    .find(|r| r.key == member.key)
                    .and_then(live_target)
            {
                member.step = Step::Terminate(target);
            }
        }
    }

    /// Give up on members that never became inactive.
    pub fn time_out(&mut self) {
        for member in &mut self.members {
            if matches!(member.step, Step::AwaitInactive | Step::Terminating) {
                member.step = Step::Done(TargetOutcome::TimedOut);
            }
        }
        self.advance_delete();
    }

    /// A command's outcome became unknown (the transport dropped).
    pub fn on_unknown(&mut self, operation: &OperationId, reason: &str) {
        self.on_result(
            operation,
            &OrgOutcome::Unknown {
                reason: reason.to_owned(),
            },
        );
    }

    /// Whether every step has ended.
    #[must_use]
    pub const fn is_done(&self) -> bool {
        matches!(self.delete, DeleteStep::Done(_))
    }

    /// Whether commands are in flight.
    #[must_use]
    pub fn has_in_flight(&self) -> bool {
        !self.issued.is_empty()
    }

    /// The per-target result so far.
    #[must_use]
    pub fn report(&self) -> KillReport {
        KillReport {
            session: self.session,
            targets: self
                .members
                .iter()
                .map(|member| {
                    let outcome = match &member.step {
                        Step::Done(outcome) => outcome.clone(),
                        _ => TargetOutcome::Unknown("the workflow did not finish".to_owned()),
                    };
                    (member.key, outcome)
                })
                .collect(),
            session_outcome: match &self.delete {
                DeleteStep::Done(outcome) => outcome.clone(),
                _ => SessionOutcome::Unknown("the workflow did not finish".to_owned()),
            },
        }
    }

    fn member_mut(&mut self, key: &ResourceKey) -> Option<&mut Member> {
        self.members.iter_mut().find(|member| member.key == *key)
    }

    fn track_session_revision(&mut self, committed: &Committed) {
        let session = self.session.as_bytes();
        for pin in &committed.revisions {
            if &pin.key == session {
                self.session_revision = pin.revision;
            }
        }
    }

    fn terminate_result(&mut self, key: ResourceKey, outcome: &OrgOutcome) {
        let Some(member) = self.member_mut(&key) else {
            return;
        };
        if !matches!(member.step, Step::Terminating) {
            return;
        }
        member.step = match outcome {
            OrgOutcome::Accepted | OrgOutcome::Committed(_) | OrgOutcome::Value(_) => {
                Step::AwaitInactive
            }
            // The activation it named already ended; a refreshed record says
            // whether a newer one runs.
            OrgOutcome::Refused(Refusal::Stale { .. }) => Step::AwaitInactive,
            OrgOutcome::Refused(Refusal::NotFound) => Step::Done(TargetOutcome::AlreadyGone),
            OrgOutcome::Refused(refusal) => Step::Done(TargetOutcome::Refused(refusal.clone())),
            OrgOutcome::Error { message, .. } => {
                Step::Done(TargetOutcome::Unknown(message.clone()))
            }
            OrgOutcome::Unknown { reason } | OrgOutcome::NotSent { reason } => {
                Step::Done(TargetOutcome::Unknown(reason.clone()))
            }
        };
    }

    fn remove_result(&mut self, key: ResourceKey, outcome: &OrgOutcome) {
        let Some(member) = self.member_mut(&key) else {
            return;
        };
        let Step::Removing { retries } = member.step else {
            return;
        };
        member.step = match outcome {
            OrgOutcome::Committed(_) | OrgOutcome::Accepted | OrgOutcome::Value(_) => {
                Step::Done(TargetOutcome::Removed)
            }
            OrgOutcome::Refused(Refusal::NotFound) => Step::Done(TargetOutcome::AlreadyGone),
            OrgOutcome::Refused(Refusal::Conflict { current })
                if retries < MAX_CONFLICT_RETRIES =>
            {
                Step::Remove {
                    expected: *current,
                    retries: retries + 1,
                }
            }
            OrgOutcome::Refused(refusal) => Step::Done(TargetOutcome::Refused(refusal.clone())),
            OrgOutcome::Error { message, .. } => {
                Step::Done(TargetOutcome::Unknown(message.clone()))
            }
            OrgOutcome::Unknown { reason } | OrgOutcome::NotSent { reason } => {
                Step::Done(TargetOutcome::Unknown(reason.clone()))
            }
        };
    }

    fn delete_result(&mut self, outcome: &OrgOutcome) {
        let DeleteStep::Deleting { retries } = self.delete else {
            return;
        };
        self.delete = match outcome {
            OrgOutcome::Committed(_) | OrgOutcome::Accepted | OrgOutcome::Value(_) => {
                DeleteStep::Done(SessionOutcome::Deleted)
            }
            OrgOutcome::Refused(Refusal::NotFound) => DeleteStep::Done(SessionOutcome::AlreadyGone),
            // `NOT_EMPTY` is checked before the revision, so a conflict means
            // the session is still empty and only its revision moved.
            OrgOutcome::Refused(Refusal::Conflict { current })
                if retries < MAX_CONFLICT_RETRIES =>
            {
                DeleteStep::Ready {
                    expected: *current,
                    retries: retries + 1,
                }
            }
            OrgOutcome::Refused(refusal) => {
                DeleteStep::Done(SessionOutcome::Refused(refusal.clone()))
            }
            OrgOutcome::Error { message, .. } => {
                DeleteStep::Done(SessionOutcome::Unknown(message.clone()))
            }
            OrgOutcome::Unknown { reason } | OrgOutcome::NotSent { reason } => {
                DeleteStep::Done(SessionOutcome::Unknown(reason.clone()))
            }
        };
    }

    fn advance_delete(&mut self) {
        if !matches!(self.delete, DeleteStep::Waiting) {
            return;
        }
        if !self.members.iter().all(|m| matches!(m.step, Step::Done(_))) {
            return;
        }
        let all_gone = self.members.iter().all(|m| match &m.step {
            Step::Done(outcome) => outcome.is_gone(),
            _ => false,
        });
        self.delete = if all_gone {
            DeleteStep::Ready {
                expected: self.session_revision,
                retries: 0,
            }
        } else {
            DeleteStep::Done(SessionOutcome::Kept)
        };
    }
}

fn initial_step(record: &ResourceRecord) -> Step {
    let state = record.activation.map(|activation| activation.state);
    if is_inactive(state) {
        return Step::Remove {
            expected: record.revision,
            retries: 0,
        };
    }
    if state == Some(ExecState::Lost) {
        return Step::Done(TargetOutcome::Refused(Refusal::ExecutionUncertain));
    }
    live_target(record).map_or(Step::Done(TargetOutcome::NoLiveTarget), Step::Terminate)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_protocol::ResourceId;
    use phux_protocol::ids::{ActivationId, ServerInstance};
    use phux_protocol::wire::frame::{ActivationInfo, RecordRevision};

    fn op(n: u8) -> OperationId {
        OperationId::new([n; 16]).unwrap()
    }

    fn skey(n: u8) -> SessionKey {
        SessionKey::new([n; 16]).unwrap()
    }

    fn rkey(n: u8) -> ResourceKey {
        ResourceKey::new([0x40 | n; 16]).unwrap()
    }

    fn rename(n: u8, name: &str) -> Command {
        Command::SessionRename {
            operation: op(n),
            session: skey(1),
            expected: Revision::new(3),
            name: name.to_owned(),
        }
    }

    fn committed(n: u8) -> CommandResult {
        CommandResult::OkWith(CommandValue::Committed(Box::new(Committed::new(
            op(n),
            Revision::new(4),
        ))))
    }

    fn sequence() -> impl FnMut() -> u32 {
        let mut next = 0;
        move || {
            next += 1;
            next
        }
    }

    #[test]
    fn a_reconnect_resends_the_same_operation_id_and_payload() {
        let mut journal = OrgJournal::default();
        journal.submit(rename(1, "work"), 0).unwrap();
        assert!(
            journal.next_frames(&mut sequence()).is_empty(),
            "no connection"
        );
        assert!(journal.begin_connection(true, 0).is_empty());
        let first = journal.next_frames(&mut sequence());
        assert_eq!(first.len(), 1);
        assert!(journal.next_frames(&mut sequence()).is_empty(), "in flight");

        journal.connection_lost();
        assert!(journal.begin_connection(true, 1_000).is_empty());
        let second = journal.next_frames(&mut sequence());
        let (FrameKind::Command { command: a, .. }, FrameKind::Command { command: b, .. }) =
            (&first[0], &second[0])
        else {
            panic!("commands")
        };
        assert_eq!(a, b, "the resend is byte-identical, operation id included");

        let report = journal.resolve(1, &committed(1)).unwrap();
        assert_eq!(report.operation, op(1));
        assert!(report.outcome.succeeded());
        assert!(journal.is_empty());
    }

    #[test]
    fn a_resubmit_dedupes_and_a_reused_id_with_new_payload_is_refused() {
        let mut journal = OrgJournal::default();
        journal.submit(rename(1, "work"), 0).unwrap();
        journal.submit(rename(1, "work"), 5).unwrap();
        assert_eq!(journal.len(), 1);
        assert_eq!(
            journal.submit(rename(1, "play"), 5),
            Err(SubmitError::DuplicateOperation)
        );
        assert_eq!(
            journal.submit(Command::Shutdown, 0),
            Err(SubmitError::NotAnOperation)
        );
    }

    #[test]
    fn past_the_horizon_an_attempted_operation_is_unknown_and_never_resent() {
        let mut journal = OrgJournal::new(1_000);
        journal.submit(rename(1, "work"), 0).unwrap();
        journal.submit(rename(2, "other"), 0).unwrap();
        journal.begin_connection(true, 0);
        let frames = journal.next_frames(&mut sequence());
        assert_eq!(frames.len(), 2);
        journal.resolve(2, &committed(2));
        journal.connection_lost();
        let reports = journal.begin_connection(true, 5_000);
        assert_eq!(reports.len(), 1);
        assert!(matches!(reports[0].outcome, OrgOutcome::Unknown { .. }));
        assert!(journal.next_frames(&mut sequence()).is_empty());
    }

    #[test]
    fn a_server_without_organization_strands_sent_as_unknown_and_unsent_as_not_sent() {
        let mut journal = OrgJournal::default();
        journal.submit(rename(1, "work"), 0).unwrap();
        journal.begin_connection(true, 0);
        journal.next_frames(&mut sequence());
        journal.connection_lost();
        journal.submit(rename(2, "other"), 0).unwrap();
        let reports = journal.begin_connection(false, 10);
        assert_eq!(reports.len(), 2);
        assert!(matches!(reports[0].outcome, OrgOutcome::Unknown { .. }));
        assert!(matches!(reports[1].outcome, OrgOutcome::NotSent { .. }));
    }

    #[test]
    fn another_authority_strands_attempted_operations() {
        let authority = |n| AuthorityId::new([n; 16]).unwrap();
        let mut journal = OrgJournal::default();
        journal.submit(rename(1, "work"), 0).unwrap();
        journal.begin_connection(true, 0);
        assert!(journal.set_authority(authority(1)).is_empty());
        journal.next_frames(&mut sequence());
        journal.connection_lost();
        journal.begin_connection(true, 10);
        let reports = journal.set_authority(authority(2));
        assert_eq!(reports.len(), 1);
        assert!(matches!(reports[0].outcome, OrgOutcome::Unknown { .. }));
    }

    fn instance() -> ServerInstance {
        ServerInstance::new([9; 16])
    }

    fn resource(n: u8, state: Option<ExecState>) -> ResourceRecord {
        ResourceRecord::new(rkey(n), skey(1), Revision::new(2))
            .with_activation(
                state.map(|s| ActivationInfo::new(ActivationId::new(1), instance(), s)),
            )
            .with_live(Some(ResourceId::local(u32::from(n))))
    }

    fn minting() -> impl FnMut() -> OperationId {
        let mut next = 100_u8;
        move || {
            next += 1;
            op(next)
        }
    }

    fn session() -> SessionRecord {
        SessionRecord::new(skey(1), "work", Revision::new(7)).with_members(3)
    }

    #[test]
    fn kill_terminates_waits_removes_then_deletes_with_the_tracked_revision() {
        let resources = [
            resource(1, Some(ExecState::Running)),
            resource(2, Some(ExecState::Completed)),
        ];
        let mut workflow = KillWorkflow::plan(&session(), &resources);
        let mut mint = minting();
        let first = workflow.next_commands(&mut mint);
        assert!(matches!(first[0].1, Command::ResourceTerminate { .. }));
        assert!(matches!(first[1].1, Command::ResourceRemove { .. }));
        workflow.on_result(&first[0].0, &OrgOutcome::Accepted);
        let removed =
            Committed::new(first[1].0, Revision::new(3)).with_revisions(vec![RecordRevision {
                key: *skey(1).as_bytes(),
                revision: Revision::new(8),
            }]);
        workflow.on_result(&first[1].0, &OrgOutcome::Committed(Box::new(removed)));
        assert!(
            workflow.next_commands(&mut mint).is_empty(),
            "still running"
        );
        assert_eq!(workflow.awaiting().collect::<Vec<_>>(), vec![rkey(1)]);

        workflow.on_activation(&rkey(1), ExecState::Completed);
        let remove = workflow.next_commands(&mut mint);
        assert!(
            matches!(remove[0].1, Command::ResourceRemove { .. }),
            "remove"
        );
        workflow.on_result(
            &remove[0].0,
            &OrgOutcome::Refused(Refusal::Conflict {
                current: Revision::new(5),
            }),
        );
        let retry = workflow.next_commands(&mut mint);
        assert!(matches!(retry[0].1, Command::ResourceRemove { .. }));
        workflow.on_result(
            &retry[0].0,
            &OrgOutcome::Committed(Box::new(Committed::new(retry[0].0, Revision::new(6)))),
        );
        let delete = workflow.next_commands(&mut mint);
        assert!(matches!(delete[0].1, Command::SessionDelete { .. }));
        workflow.on_result(
            &delete[0].0,
            &OrgOutcome::Committed(Box::new(Committed::new(delete[0].0, Revision::new(9)))),
        );
        assert!(workflow.is_done());
        let report = workflow.report();
        assert!(report.complete());
        assert!(report.targets.iter().all(|(_, outcome)| outcome.is_gone()));
    }

    #[test]
    fn work_added_meanwhile_makes_the_delete_refuse_not_empty() {
        let mut workflow = KillWorkflow::plan(&session(), &[resource(1, None)]);
        let mut mint = minting();
        let remove = workflow.next_commands(&mut mint);
        workflow.on_result(
            &remove[0].0,
            &OrgOutcome::Committed(Box::new(Committed::new(remove[0].0, Revision::new(3)))),
        );
        let delete = workflow.next_commands(&mut mint);
        workflow.on_result(
            &delete[0].0,
            &OrgOutcome::Refused(Refusal::NotEmpty { members: 1 }),
        );
        let report = workflow.report();
        assert_eq!(report.targets, vec![(rkey(1), TargetOutcome::Removed)]);
        assert_eq!(
            report.session_outcome,
            SessionOutcome::Refused(Refusal::NotEmpty { members: 1 })
        );
    }

    #[test]
    fn a_lost_member_keeps_the_session_and_others_still_go() {
        let resources = [
            resource(1, Some(ExecState::Lost)),
            resource(2, Some(ExecState::Completed)),
        ];
        let mut workflow = KillWorkflow::plan(&session(), &resources);
        let mut mint = minting();
        let commands = workflow.next_commands(&mut mint);
        assert_eq!(commands.len(), 1);
        workflow.on_result(&commands[0].0, &OrgOutcome::Refused(Refusal::NotFound));
        assert!(workflow.is_done());
        let report = workflow.report();
        assert_eq!(report.session_outcome, SessionOutcome::Kept);
        assert_eq!(
            report.targets[0].1,
            TargetOutcome::Refused(Refusal::ExecutionUncertain)
        );
        assert_eq!(report.targets[1].1, TargetOutcome::AlreadyGone);
    }

    #[test]
    fn records_refresh_removals_escalate_and_time_out() {
        let resources = [
            resource(1, Some(ExecState::Running)),
            resource(2, Some(ExecState::Running)),
            resource(3, Some(ExecState::Running)),
        ];
        let mut workflow = KillWorkflow::plan(&session(), &resources);
        let mut mint = minting();
        for (operation, _) in workflow.next_commands(&mut mint) {
            workflow.on_result(&operation, &OrgOutcome::Accepted);
        }
        // 1 removed by its exit policy; 2 now completed; 3 still running.
        workflow.on_records(
            Some(&session()),
            &[
                resource(2, Some(ExecState::Completed)),
                resource(3, Some(ExecState::Running)),
            ],
        );
        workflow.escalate(&resources);
        let commands = workflow.next_commands(&mut mint);
        assert!(commands.iter().any(|(_, c)| matches!(
            c,
            Command::ResourceTerminate {
                mode: TerminateMode::Force,
                ..
            }
        )));
        assert!(
            commands
                .iter()
                .any(|(_, c)| matches!(c, Command::ResourceRemove { .. }))
        );
        workflow.time_out();
        for (operation, _) in commands {
            workflow.on_result(
                &operation,
                &OrgOutcome::Committed(Box::new(Committed::new(operation, Revision::new(3)))),
            );
        }
        let report = workflow.report();
        assert_eq!(report.targets[0].1, TargetOutcome::AlreadyGone);
        assert_eq!(report.targets[1].1, TargetOutcome::Removed);
        assert_eq!(report.targets[2].1, TargetOutcome::TimedOut);
        assert_eq!(report.session_outcome, SessionOutcome::Kept);
    }

    #[test]
    fn an_empty_session_is_deleted_at_once() {
        let mut workflow = KillWorkflow::plan(&session(), &[]);
        let mut mint = minting();
        let commands = workflow.next_commands(&mut mint);
        assert!(matches!(commands[..], [(_, Command::SessionDelete { .. })]));
    }
}
