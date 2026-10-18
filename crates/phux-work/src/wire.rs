//! Keeper exchange: one JSON request line in, one JSON response line out,
//! per connection. This is a private interface between a phux binary and
//! the keeper it started, not `phux-coordinator/1`; a request from another
//! build is answered with [`Fault::Stale`] and the keeper exits.

use serde::{Deserialize, Serialize};

use crate::coord::{
    AckMail, Claim, ClaimFilter, ClaimOutcome, Mail, ReadMail, ReleaseClaim, RenewClaim, SendMail,
    Sent, TakeClaim,
};
use crate::error::Error;
use crate::id::Alias;
use crate::start::{StartApplied, StartPlan, StartRequest};
use crate::store::{
    AddObjective, Applied, EditObjective, ListFilter, Objective, RaiseSignal, RevisionEdit, Signal,
    StateEdit,
};

/// Keeper compatibility includes a semantic epoch, even during development
/// within one package version. Bump the epoch for incompatible request or
/// mutation semantics; otherwise an already-running keeper could retain them.
pub(crate) const BUILD: &str = concat!(env!("CARGO_PKG_VERSION"), "/keeper-2");

/// Longest request or response line, in bytes.
pub(crate) const MAX_LINE: u64 = 1 << 20;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Envelope {
    pub(crate) build: String,
    pub(crate) request: Request,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum Request {
    Add(AddObjective),
    List(ListFilter),
    Show { alias: Alias },
    Edit(EditObjective),
    Archive(RevisionEdit),
    Restore(RevisionEdit),
    RecordState(StateEdit),
    RaiseSignal(RaiseSignal),
    Signals { alias: Alias },
    PlanStart(StartRequest),
    ReplayStart(StartRequest),
    CommitStart(StartRequest),
    SendMail(SendMail),
    ReadMail(ReadMail),
    AckMail(AckMail),
    TakeClaim(TakeClaim),
    RenewClaim(RenewClaim),
    ReleaseClaim(ReleaseClaim),
    Claims(ClaimFilter),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
pub(crate) enum Response {
    Applied(Applied),
    Objective(Objective),
    Objectives(Vec<Objective>),
    Signals(Vec<Signal>),
    StartPlan(StartPlan),
    StartReplay(Option<StartApplied>),
    StartApplied(StartApplied),
    Sent(Sent),
    Mail(Vec<Mail>),
    Acked(Mail),
    ClaimOutcome(ClaimOutcome),
    Released(u64),
    Claims(Vec<Claim>),
    Fault(Fault),
}

/// An [`Error`] as it crosses the socket. Store-internal failures travel as
/// their message; everything a caller branches on keeps its fields.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Fault {
    NotFound {
        alias: String,
    },
    RevisionConflict {
        alias: String,
        expected: u64,
        actual: u64,
    },
    OperationConflict {
        operation_id: String,
    },
    UnsupportedSchema {
        found: i64,
        supported: i64,
    },
    InvalidAlias {
        text: String,
    },
    InvalidOperationId {
        text: String,
    },
    InvalidRepo {
        text: String,
    },
    InvalidText {
        field: String,
        reason: String,
    },
    EmptyEdit,
    NotStartable {
        alias: String,
    },
    AlreadyStarted {
        alias: String,
    },
    WorktreeBound {
        path: String,
        alias: String,
    },
    AtCapacity {
        running: u64,
        max_agents: u64,
    },
    NoMail {
        id: String,
        reader: String,
    },
    NoClaim {
        fence: u64,
    },
    Busy,
    Stale {
        keeper: String,
    },
    BadRequest {
        message: String,
        /// Absent only in legacy keepers that decode the payload before the header.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        keeper: Option<String>,
    },
    Store {
        message: String,
    },
}

impl From<Error> for Fault {
    fn from(err: Error) -> Self {
        match err {
            Error::NotFound { alias } => Self::NotFound { alias },
            Error::RevisionConflict {
                alias,
                expected,
                actual,
            } => Self::RevisionConflict {
                alias,
                expected,
                actual,
            },
            Error::OperationConflict { operation_id } => Self::OperationConflict { operation_id },
            Error::UnsupportedSchema { found, supported } => {
                Self::UnsupportedSchema { found, supported }
            }
            Error::InvalidAlias(text) => Self::InvalidAlias { text },
            Error::InvalidOperationId(text) => Self::InvalidOperationId { text },
            Error::InvalidRepo(text) => Self::InvalidRepo { text },
            Error::InvalidText { field, reason } => Self::InvalidText { field, reason },
            Error::EmptyEdit => Self::EmptyEdit,
            Error::NotStartable { alias } => Self::NotStartable { alias },
            Error::AlreadyStarted { alias } => Self::AlreadyStarted { alias },
            Error::WorktreeBound { path, alias } => Self::WorktreeBound { path, alias },
            Error::AtCapacity { running, limit } => Self::AtCapacity {
                running,
                max_agents: limit,
            },
            Error::NoMail { id, reader } => Self::NoMail { id, reader },
            Error::NoClaim { fence } => Self::NoClaim { fence },
            Error::Busy => Self::Busy,
            other @ (Error::Io(_)
            | Error::Sqlite(_)
            | Error::Json(_)
            | Error::Random(_)
            | Error::Corrupt(_)
            | Error::Keeper(_)
            | Error::Store(_)) => Self::Store {
                message: other.to_string(),
            },
        }
    }
}

impl From<Fault> for Error {
    fn from(fault: Fault) -> Self {
        match fault {
            Fault::NotFound { alias } => Self::NotFound { alias },
            Fault::RevisionConflict {
                alias,
                expected,
                actual,
            } => Self::RevisionConflict {
                alias,
                expected,
                actual,
            },
            Fault::OperationConflict { operation_id } => Self::OperationConflict { operation_id },
            Fault::UnsupportedSchema { found, supported } => {
                Self::UnsupportedSchema { found, supported }
            }
            Fault::InvalidAlias { text } => Self::InvalidAlias(text),
            Fault::InvalidOperationId { text } => Self::InvalidOperationId(text),
            Fault::InvalidRepo { text } => Self::InvalidRepo(text),
            Fault::InvalidText { field, reason } => Self::InvalidText { field, reason },
            Fault::EmptyEdit => Self::EmptyEdit,
            Fault::NotStartable { alias } => Self::NotStartable { alias },
            Fault::AlreadyStarted { alias } => Self::AlreadyStarted { alias },
            Fault::WorktreeBound { path, alias } => Self::WorktreeBound { path, alias },
            Fault::AtCapacity {
                running,
                max_agents,
            } => Self::AtCapacity {
                running,
                limit: max_agents,
            },
            Fault::NoMail { id, reader } => Self::NoMail { id, reader },
            Fault::NoClaim { fence } => Self::NoClaim { fence },
            Fault::Busy => Self::Busy,
            Fault::Stale { keeper } => Self::Keeper(format!("keeper is from build {keeper}")),
            Fault::BadRequest { message, .. } => Self::Keeper(message),
            Fault::Store { message } => Self::Store(message),
        }
    }
}
