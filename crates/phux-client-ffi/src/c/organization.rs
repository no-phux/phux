//! The organization surface of the C ABI: records and typed operations,
//! over the runtime's organization replica and operation journal
//! (`docs/organization.md`, CONTRACTS §04-§06).
//!
//! The runtime owns the replica and the journal (one `OperationId` per
//! action, resent with the same id after a reconnect inside the retry
//! horizon). This module only correlates host request ids with the
//! operations they submitted, and keeps the borrowed views the C records
//! point into. The client seeds the replica once per connection that
//! advertises `ORGANIZATION` (its control plane runs with
//! `automatic_lifecycle` off, so nothing else would).
#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared by the bridge dispatcher and Client"
)]

use std::collections::BTreeMap;
use std::mem;

use phux_client_core::org_ops::{OrgOutcome, SubmitError};
use phux_client_runtime::control::{ControlPlane, Event, ReplicaState};
use phux_protocol::ResourceId;
use phux_protocol::ids::{ArrangementKey, OperationId, ResourceKey, SessionKey};
use phux_protocol::wire::frame::{
    Committed, ExecState, ExitPolicy, Outcome, ResourceRecord, SessionLifetime, TerminateMode,
};

use crate::c::client::Client;
use crate::c::error::{BridgeError, check_struct};
use crate::c::types::{PhuxBytes, PhuxResourceId, bytes_out};
use crate::c::{PhuxClient, PhuxClientResult, with_client_mut, with_client_ref};
use crate::projection::organization::{Resolution, Status, TargetResult};

/// Most resources one `phux_client_org_move_many` names.
pub const MAX_MOVE_MANY: usize = 1024;

mod operations;
mod records;
mod results;
mod state;
mod submission;

pub use operations::*;
pub use records::*;
pub use results::*;
pub(crate) use state::*;
pub(crate) use submission::*;

// ----- C vocabulary ---------------------------------------------------------

/// `PhuxSessionLifetime`: unknown (a server that reports none).
pub const LIFETIME_UNKNOWN: u32 = 0;
/// `PhuxSessionLifetime`: survives empty.
pub const LIFETIME_PERSISTENT: u32 = 1;
/// `PhuxSessionLifetime`: deleted with its last member.
pub const LIFETIME_EPHEMERAL: u32 = 2;

/// `PHUX_SESSION_RECORD_FOCUSED`.
pub const PHUX_SESSION_RECORD_FOCUSED: u32 = 0x1;
/// `PHUX_SESSION_RECORD_AWAITING_ORGANIZATION`.
pub const PHUX_SESSION_RECORD_AWAITING_ORGANIZATION: u32 = 0x2;
/// The designated Intake session, with the existing ABI flag value.
pub const PHUX_SESSION_RECORD_INTAKE: u32 = 0x4;
/// Legacy ABI spelling of [`PHUX_SESSION_RECORD_INTAKE`].
pub const PHUX_SESSION_RECORD_INBOX: u32 = PHUX_SESSION_RECORD_INTAKE;
/// `PHUX_SESSION_RECORD_ORGANIZED`: the replica holds the record, so
/// `revision`, `arrangement_key`, and the state counts are its values.
pub const PHUX_SESSION_RECORD_ORGANIZED: u32 = 0x8;
/// `PHUX_SESSION_RECORD_EMPTY`.
pub const PHUX_SESSION_RECORD_EMPTY: u32 = 0x10;

/// `PhuxOrgKind` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Move = 1,
    MoveMany = 2,
    Terminate = 3,
    Remove = 4,
    Relaunch = 5,
    DeleteSession = 6,
    SetLifetime = 7,
    IntakeResolve = 8,
    Undo = 9,
    KillSession = 10,
}

impl Kind {
    /// Whether the server keeps an inverse for this edit (CONTRACTS §06).
    const fn undoable(self) -> bool {
        matches!(self, Self::Move | Self::MoveMany | Self::IntakeResolve)
    }
}

/// The C value of a lifetime.
#[must_use]
pub const fn lifetime_code(lifetime: Option<SessionLifetime>) -> u32 {
    match lifetime {
        None => LIFETIME_UNKNOWN,
        Some(SessionLifetime::Persistent) => LIFETIME_PERSISTENT,
        Some(SessionLifetime::Ephemeral) => LIFETIME_EPHEMERAL,
    }
}

/// A lifetime from its C value.
pub(crate) fn lifetime_in(code: u32) -> Result<SessionLifetime, BridgeError> {
    match code {
        LIFETIME_PERSISTENT => Ok(SessionLifetime::Persistent),
        LIFETIME_EPHEMERAL => Ok(SessionLifetime::Ephemeral),
        _ => Err(BridgeError::invalid(
            "lifetime must be PHUX_SESSION_LIFETIME_PERSISTENT or _EPHEMERAL",
        )),
    }
}

/// `PhuxExecState`: `0` when there is no activation.
#[must_use]
pub const fn exec_code(state: Option<ExecState>) -> u32 {
    match state {
        None => 0,
        Some(ExecState::Starting) => 1,
        Some(ExecState::Running) => 2,
        Some(ExecState::Stopping) => 3,
        Some(ExecState::Completed) => 4,
        Some(ExecState::Failed) => 5,
        Some(ExecState::Lost) => 6,
        Some(ExecState::Abandoned) => 7,
    }
}

/// `PhuxOutcomeKind` and its code: none, exited, signaled, unobserved,
/// other.
#[must_use]
pub const fn outcome_code(outcome: Option<Outcome>) -> (u32, i32) {
    match outcome {
        None => (0, 0),
        Some(Outcome::Exited(code)) => (1, code),
        Some(Outcome::Signaled(signal)) => (2, signal),
        Some(Outcome::Unobserved) => (3, 0),
        #[allow(
            unreachable_patterns,
            reason = "Outcome is non_exhaustive across crates"
        )]
        Some(_) => (4, 0),
    }
}

// ----- records ----------------------------------------------------------------

/// What the organization replica adds to one session of the snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SessionOrg {
    /// The replica holds this session's record.
    pub known: bool,
    pub revision: u64,
    pub members: u32,
    pub arrangement: Option<ArrangementKey>,
    pub awaiting_organization: bool,
    pub inbox: bool,
    /// Members whose activation is starting, running, or stopping.
    pub running: u32,
    /// Members whose activation completed, failed, or was abandoned.
    pub completed: u32,
    /// Members whose activation is lost.
    pub lost: u32,
}

/// One resource record and the storage its C view borrows.
#[derive(Debug)]
pub(crate) struct RecordView {
    pub record: ResourceRecord,
    pub session_id: u32,
    key_text: Vec<u8>,
    name: Vec<u8>,
    live_host: Vec<u8>,
}

impl RecordView {
    fn new(record: ResourceRecord, session_id: u32) -> Self {
        let live_host = match &record.live {
            Some(ResourceId::Satellite { host, .. }) => host.as_str().as_bytes().to_vec(),
            _ => Vec::new(),
        };
        Self {
            key_text: record.key.to_string().into_bytes(),
            name: record.name.clone().unwrap_or_default().into_bytes(),
            record,
            session_id,
            live_host,
        }
    }

    fn live_view(&self) -> PhuxResourceId {
        match &self.record.live {
            None => PhuxResourceId::default(),
            Some(ResourceId::Local { id }) => PhuxResourceId {
                kind: 0,
                id: *id,
                host: PhuxBytes::default(),
            },
            Some(ResourceId::Satellite { id, .. }) => PhuxResourceId {
                kind: 1,
                id: *id,
                host: bytes_out(&self.live_host),
            },
        }
    }
}

// ----- requests -----------------------------------------------------------------

/// One submitted operation of a host request.
#[derive(Debug)]
struct Part {
    operation: OperationId,
    /// The records this part addresses (a chunk of a bulk move).
    keys: Vec<[u8; 16]>,
    is_session: bool,
    resolution: Resolution,
    receipt: Option<Committed>,
}

/// One host request and its retained result.
#[derive(Debug)]
struct Request {
    kind: Kind,
    parts: Vec<Part>,
    view: ResultView,
}

#[derive(Debug, Default)]
struct ResultView {
    status: u32,
    refusal: u32,
    undoable: bool,
    revision: u64,
    conflict_revision: u64,
    message: Vec<u8>,
    targets: Vec<TargetView>,
}

#[derive(Debug)]
struct TargetView {
    key: [u8; 16],
    is_session: bool,
    status: u32,
    refusal: u32,
    message: Vec<u8>,
}

impl TargetView {
    fn from_result(result: &TargetResult) -> Self {
        Self {
            key: result.key,
            is_session: result.is_session,
            status: result.status.code(),
            refusal: result.refusal,
            message: result.message.as_bytes().to_vec(),
        }
    }
}

impl Request {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            parts: Vec::new(),
            view: ResultView::default(),
        }
    }

    fn pending(&self) -> bool {
        self.parts
            .iter()
            .any(|part| part.resolution.status == Status::Pending)
    }

    /// Recompute the borrowed result view from the parts.
    fn refresh_view(&mut self) {
        let statuses: Vec<Status> = self
            .parts
            .iter()
            .map(|part| part.resolution.status)
            .collect();
        let status = aggregate(&statuses);
        let first_failure = self.parts.iter().find(|part| {
            !part.resolution.status.succeeded() && part.resolution.status != Status::Pending
        });
        let targets = self
            .parts
            .iter()
            .flat_map(|part| {
                part.keys.iter().map(move |key| {
                    let mut target = TargetResult::resource(*key, &part.resolution);
                    target.is_session = part.is_session;
                    TargetView::from_result(&target)
                })
            })
            .collect();
        self.view = ResultView {
            status: status.code(),
            refusal: first_failure.map_or(0, |part| part.resolution.refusal),
            undoable: self.kind.undoable()
                && !self.parts.is_empty()
                && self.parts.iter().all(|part| part.receipt.is_some()),
            revision: match self.parts.as_slice() {
                [only] => only.resolution.revision.unwrap_or(0),
                _ => 0,
            },
            conflict_revision: self
                .parts
                .iter()
                .find_map(|part| part.resolution.conflict_revision)
                .unwrap_or(0),
            message: first_failure
                .map(|part| part.resolution.message.as_bytes().to_vec())
                .unwrap_or_default(),
            targets,
        };
    }
}

/// The status of a request made of `parts`.
fn aggregate(parts: &[Status]) -> Status {
    if parts.is_empty() {
        return Status::Pending;
    }
    if parts.contains(&Status::Pending) {
        return Status::Pending;
    }
    let succeeded = parts.iter().filter(|status| status.succeeded()).count();
    if succeeded == parts.len() {
        return if parts.iter().all(|status| *status == Status::Accepted) {
            Status::Accepted
        } else {
            Status::Committed
        };
    }
    if succeeded > 0 {
        return Status::Partial;
    }
    parts[0]
}

#[cfg(test)]
mod tests;
