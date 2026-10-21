//! Transport-free organization operations: the retry journal.
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
//! [`InputReplayJournal`]: crate::input_replay::InputReplayJournal

use std::collections::VecDeque;

use phux_protocol::ids::{AuthorityId, OperationId};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, Committed, ErrorCode, ExecState, FrameKind, LiveTarget,
    Refusal, ResourceRecord,
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
    /// The connected server does not accept this command.
    #[error("the server does not accept this command")]
    Unsupported,
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

#[cfg(test)]
mod tests;
