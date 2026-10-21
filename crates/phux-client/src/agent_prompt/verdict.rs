//! How an `APPLY_INPUT` reply reads: delivery, refusal, and verdict.

use phux_protocol::wire::frame::{CommandResult, ErrorCode};

/// How the batch ended up, as reported in `--json`'s `delivery` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Every byte was accepted into the pane's tty input queue.
    Acked,
    /// Indeterminate: some, all, or none of the bytes may have reached the
    /// tty. Terminal — never retried.
    Unknown,
    /// Nothing was written.
    Refused,
}

impl Delivery {
    /// The wire-facing word for this outcome.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Acked => "acked",
            Self::Unknown => "unknown",
            Self::Refused => "refused",
        }
    }
}

/// A refusal in which nothing was written and which resubmitting the
/// identical batch cannot fix (exit 2). `RESOURCE_EXHAUSTED` is
/// [`ApplyVerdict::Busy`] instead: it can succeed unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The prompt text is empty; there is nothing to submit.
    EmptyText,
    /// The prompt text carries a raw newline (ADR-0076 point 3): without DEC
    /// 2004, which no client can observe, each would become a submission.
    MultilineText {
        /// How many raw newlines the text carries.
        newlines: usize,
    },
    /// The payload is over a size ceiling; `wire` marks a protocol cap.
    TooLarge {
        /// What was measured (`"bytes"` or `"events"`).
        unit: &'static str,
        /// The measured size.
        measured: usize,
        /// The ceiling it exceeded.
        limit: usize,
        /// Whether `limit` is a protocol cap rather than a client policy.
        wire: bool,
    },
    /// The target is a federation satellite; `APPLY_INPUT` is local-only and
    /// is never downgraded to `ROUTE_INPUT`.
    SatelliteTarget {
        /// The satellite host token the target named.
        host: String,
    },
    /// The server does not advertise `ACKNOWLEDGED_INPUT`.
    NoAcknowledgedInput,
    /// The pane declares no `phux.agent/v1` record to verify against.
    NoAgentRecord,
    /// The pane hosts a different agent than the caller named; the string
    /// describes the occupant.
    AgentMismatch(String),
    /// Another client holds the pane's input lease (ADR-0033); these verbs
    /// never seize it.
    InputLeaseHeld(String),
    /// A canonical-mode pane would have silently truncated a batch with no
    /// terminator; refused before any byte was written.
    CanonicalLimitExceeded(String),
    /// An untrusted paste failed the pane's policy (unreachable here: these
    /// verbs send `Trusted`).
    UnsafePaste(String),
    /// The server rejected the batch structurally, or the operation id names
    /// input that differs from this batch.
    InvalidBatch(String),
    /// The server forbade the operation for this peer.
    PermissionDenied(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyText => f.write_str("the prompt text is empty"),
            Self::MultilineText { newlines } => write!(
                f,
                "the prompt text carries {newlines} raw newline(s); a pane that has not set \
                 bracketed-paste mode turns each one into a submission, and no client can \
                 observe that mode"
            ),
            Self::TooLarge {
                unit,
                measured,
                limit,
                wire,
            } => {
                let which = if *wire { "protocol cap" } else { "ceiling" };
                write!(
                    f,
                    "batch is {measured} {unit}, over the {limit}-{unit} {which}"
                )
            }
            Self::SatelliteTarget { host } => write!(
                f,
                "'{host}' is a federation satellite, and acknowledged input is local-only: \
                 the delivery receipt is owned by the machine that owns the PTY, and a hub \
                 cannot vouch for it"
            ),
            Self::NoAcknowledgedInput => f.write_str(
                "this server does not advertise ACKNOWLEDGED_INPUT, so there is no \
                 acknowledged input path to use",
            ),
            Self::NoAgentRecord => f.write_str(
                "the pane declares no phux.agent/v1 record, so there is no agent identity \
                 to verify",
            ),
            Self::AgentMismatch(who) => write!(f, "the pane is hosting {who}"),
            Self::InputLeaseHeld(message) => write!(f, "another client holds input: {message}"),
            Self::CanonicalLimitExceeded(message) => write!(
                f,
                "the pane refused the batch before writing any byte: {message}"
            ),
            Self::UnsafePaste(message) => {
                write!(f, "the paste failed the pane's policy: {message}")
            }
            Self::InvalidBatch(message) => write!(f, "the server rejected the batch: {message}"),
            Self::PermissionDenied(message) => write!(f, "permission denied: {message}"),
        }
    }
}

/// The one reading an `APPLY_INPUT` reply has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyVerdict {
    /// Every byte was accepted into the tty input queue and flushed.
    Acked,
    /// The server-wide lane was busy; nothing written, retried under the
    /// same operation id.
    Busy(String),
    /// Nothing was written for another proven reason (no PTY, writer queue
    /// full or closed). Not auto-retried, but safe to resubmit.
    NotWritten(String),
    /// Nothing was written and the identical batch cannot succeed.
    Refused(Refusal),
    /// The Terminal is gone or was never here.
    NotFound(String),
    /// Delivery is indeterminate. Terminal: a same-id retry replays it and a
    /// new-id retry duplicates.
    Unknown(String),
}

/// Map one `APPLY_INPUT` reply onto its single reading.
///
/// `INTERNAL_ERROR`, `INPUT_DELIVERY_UNKNOWN`, and any code or shape this
/// build cannot name pessimize to [`ApplyVerdict::Unknown`]: an optimistic
/// default is how a duplicate gets written.
#[must_use]
pub fn classify(result: &CommandResult) -> ApplyVerdict {
    let (code, message) = match result {
        CommandResult::Ok | CommandResult::OkWith(_) => return ApplyVerdict::Acked,
        CommandResult::Error { code, message } => (*code, message.clone()),
        _ => {
            return ApplyVerdict::Unknown(
                "the server answered APPLY_INPUT with a result this build cannot read".to_owned(),
            );
        }
    };
    match code {
        ErrorCode::ResourceExhausted => ApplyVerdict::Busy(message),
        ErrorCode::InputNotWritten => ApplyVerdict::NotWritten(message),
        ErrorCode::InputLeaseHeld => ApplyVerdict::Refused(Refusal::InputLeaseHeld(message)),
        ErrorCode::CanonicalLimitExceeded => {
            ApplyVerdict::Refused(Refusal::CanonicalLimitExceeded(message))
        }
        ErrorCode::UnsafePaste => ApplyVerdict::Refused(Refusal::UnsafePaste(message)),
        ErrorCode::InvalidCommand | ErrorCode::MalformedMessage | ErrorCode::UnknownMessageType => {
            ApplyVerdict::Refused(Refusal::InvalidBatch(message))
        }
        ErrorCode::PermissionDenied => ApplyVerdict::Refused(Refusal::PermissionDenied(message)),
        ErrorCode::UnsupportedSatelliteRoute | ErrorCode::SatelliteUnreachable => {
            ApplyVerdict::Refused(Refusal::SatelliteTarget { host: message })
        }
        ErrorCode::TerminalNotFound => ApplyVerdict::NotFound(message),
        _ => ApplyVerdict::Unknown(message),
    }
}
