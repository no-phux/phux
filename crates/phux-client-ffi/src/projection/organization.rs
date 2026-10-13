//! Organization outcomes, read once for every encoder.
//!
//! An organization command resolves to an [`OrgOutcome`]; a kill-session
//! workflow resolves to a [`KillReport`] with one result per member. Both
//! fold here into a status, a refusal code, and a sentence, so the C ABI,
//! `UniFFI`, and NAPI lanes report the same thing for the same answer. The
//! numeric codes are the C ABI's (`include/phux/client.h`,
//! `PhuxOrgStatus` and `PhuxOrgRefusal`); the words are what the string
//! encoders carry.

use phux_client_core::org_ops::{
    KillReport, OrgOutcome, SessionOutcome, TargetOutcome, refusal_sentence,
};
use phux_protocol::wire::frame::Refusal;

/// Where an organization operation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Submitted; no answer yet.
    Pending,
    /// The edit committed.
    Committed,
    /// Accepted; its execution effects follow as events.
    Accepted,
    /// The server refused it; nothing changed.
    Refused,
    /// The server answered with a legacy error.
    Error,
    /// It may have reached the server; reconcile before acting again.
    Unknown,
    /// It never reached a socket.
    NotSent,
    /// A batch or workflow in which some targets succeeded and some did not.
    Partial,
}

impl Status {
    /// The C ABI's `PhuxOrgStatus` value.
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::Pending => 1,
            Self::Committed => 2,
            Self::Accepted => 3,
            Self::Refused => 4,
            Self::Error => 5,
            Self::Unknown => 6,
            Self::NotSent => 7,
            Self::Partial => 8,
        }
    }

    /// The stable word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Committed => "committed",
            Self::Accepted => "accepted",
            Self::Refused => "refused",
            Self::Error => "error",
            Self::Unknown => "unknown",
            Self::NotSent => "not_sent",
            Self::Partial => "partial",
        }
    }

    /// Whether the operation is known to have taken effect.
    #[must_use]
    pub const fn succeeded(self) -> bool {
        matches!(self, Self::Committed | Self::Accepted)
    }
}

/// The C ABI's `PhuxOrgRefusal` value: the wire refusal tag plus one, `0`
/// for none, `255` for a tag this build does not name.
#[must_use]
pub const fn refusal_code(refusal: &Refusal) -> u32 {
    match refusal {
        Refusal::Unauthorized { .. } => 1,
        Refusal::NotFound => 2,
        Refusal::Conflict { .. } => 3,
        Refusal::Stale { .. } => 4,
        Refusal::NotEmpty { .. } => 5,
        Refusal::Unsupported { .. } => 6,
        Refusal::InvalidArgument { .. } => 7,
        Refusal::NameTaken => 8,
        Refusal::ExecutionUncertain => 9,
        Refusal::LeaseHeld { .. } => 10,
        Refusal::DuplicateOperation => 11,
        Refusal::StoreFailure { .. } => 12,
        _ => 255,
    }
}

/// The current revision a `Conflict` names, for a compare-and-swap caller
/// that re-reads and redoes.
#[must_use]
pub const fn conflict_revision(refusal: &Refusal) -> Option<u64> {
    match refusal {
        Refusal::Conflict { current } => Some(current.get()),
        _ => None,
    }
}

/// One operation's resolution, binding-neutral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// Where it is.
    pub status: Status,
    /// [`refusal_code`], `0` unless refused.
    pub refusal: u32,
    /// The refusal's stable word (`Refusal::as_str`), empty unless refused.
    pub refusal_word: &'static str,
    /// The record's current revision when the refusal is a conflict.
    pub conflict_revision: Option<u64>,
    /// The primary record's resulting revision when it committed.
    pub revision: Option<u64>,
    /// A sentence for a frontend; empty on success.
    pub message: String,
}

impl Resolution {
    /// Still waiting for the server.
    #[must_use]
    pub const fn pending() -> Self {
        Self {
            status: Status::Pending,
            refusal: 0,
            refusal_word: "",
            conflict_revision: None,
            revision: None,
            message: String::new(),
        }
    }

    /// A refusal decided before anything was sent.
    #[must_use]
    pub fn not_sent(message: impl Into<String>) -> Self {
        Self {
            status: Status::NotSent,
            message: message.into(),
            ..Self::pending()
        }
    }

    fn refused(refusal: &Refusal) -> Self {
        Self {
            status: Status::Refused,
            refusal: refusal_code(refusal),
            refusal_word: refusal.as_str(),
            conflict_revision: conflict_revision(refusal),
            message: refusal_sentence(refusal),
            ..Self::pending()
        }
    }
}

/// Read one operation's outcome.
#[must_use]
pub fn resolution(outcome: &OrgOutcome) -> Resolution {
    match outcome {
        OrgOutcome::Committed(committed) => Resolution {
            status: Status::Committed,
            revision: Some(committed.revision.get()),
            ..Resolution::pending()
        },
        OrgOutcome::Accepted | OrgOutcome::Value(_) => Resolution {
            status: Status::Accepted,
            ..Resolution::pending()
        },
        OrgOutcome::Refused(refusal) => Resolution::refused(refusal),
        OrgOutcome::Error { message, .. } => Resolution {
            status: Status::Error,
            message: message.clone(),
            ..Resolution::pending()
        },
        OrgOutcome::Unknown { reason } => Resolution {
            status: Status::Unknown,
            message: format!("outcome unknown: {reason}"),
            ..Resolution::pending()
        },
        OrgOutcome::NotSent { reason } => Resolution::not_sent(format!("not sent: {reason}")),
    }
}

/// How one target of a workflow or batch ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetStatus {
    /// Still in flight.
    Pending,
    /// It took effect: moved, removed, deleted.
    Done,
    /// It was gone before the operation reached it.
    AlreadyGone,
    /// The server refused a step.
    Refused,
    /// A step's outcome is unknown.
    Unknown,
    /// It did not become inactive in time.
    TimedOut,
    /// It executes on another server and cannot be terminated from here.
    NoLiveTarget,
    /// A member could not be removed, so the session delete was not sent.
    Kept,
    /// It was never sent.
    NotSent,
}

impl TargetStatus {
    /// The C ABI's `PhuxOrgTargetStatus` value.
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            Self::Pending => 1,
            Self::Done => 2,
            Self::AlreadyGone => 3,
            Self::Refused => 4,
            Self::Unknown => 5,
            Self::TimedOut => 6,
            Self::NoLiveTarget => 7,
            Self::Kept => 8,
            Self::NotSent => 9,
        }
    }

    /// The stable word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::AlreadyGone => "already_gone",
            Self::Refused => "refused",
            Self::Unknown => "unknown",
            Self::TimedOut => "timed_out",
            Self::NoLiveTarget => "no_live_target",
            Self::Kept => "kept",
            Self::NotSent => "not_sent",
        }
    }

    /// Whether the target no longer stands in the way.
    #[must_use]
    pub const fn succeeded(self) -> bool {
        matches!(self, Self::Done | Self::AlreadyGone)
    }
}

/// One target's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetResult {
    /// The record's key bytes.
    pub key: [u8; 16],
    /// Whether the target is a session (`true`) or a resource.
    pub is_session: bool,
    /// How it ended.
    pub status: TargetStatus,
    /// [`refusal_code`], `0` unless refused.
    pub refusal: u32,
    /// A sentence for a frontend; empty on success.
    pub message: String,
}

impl TargetResult {
    /// A resource target, resolved from an operation's outcome.
    #[must_use]
    pub fn resource(key: [u8; 16], resolution: &Resolution) -> Self {
        let status = match resolution.status {
            Status::Pending => TargetStatus::Pending,
            Status::Committed | Status::Accepted => TargetStatus::Done,
            Status::Refused | Status::Error => TargetStatus::Refused,
            Status::Unknown | Status::Partial => TargetStatus::Unknown,
            Status::NotSent => TargetStatus::NotSent,
        };
        Self {
            key,
            is_session: false,
            status,
            refusal: resolution.refusal,
            message: resolution.message.clone(),
        }
    }
}

/// A kill-session workflow's report, binding-neutral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillResult {
    /// The overall status: committed when the session went away, partial
    /// when some members were removed and the session stayed, refused or
    /// unknown otherwise.
    pub status: Status,
    /// The session's own result, first, then each member in membership
    /// order.
    pub targets: Vec<TargetResult>,
    /// A sentence for a frontend.
    pub message: String,
}

fn target_of(outcome: &TargetOutcome) -> (TargetStatus, u32, String) {
    match outcome {
        TargetOutcome::Removed => (TargetStatus::Done, 0, String::new()),
        TargetOutcome::AlreadyGone => (TargetStatus::AlreadyGone, 0, String::new()),
        TargetOutcome::Refused(refusal) => (
            TargetStatus::Refused,
            refusal_code(refusal),
            refusal_sentence(refusal),
        ),
        TargetOutcome::Unknown(reason) => (TargetStatus::Unknown, 0, reason.clone()),
        TargetOutcome::TimedOut => (
            TargetStatus::TimedOut,
            0,
            "it did not stop in time".to_owned(),
        ),
        TargetOutcome::NoLiveTarget => (
            TargetStatus::NoLiveTarget,
            0,
            "it is not executing on this server".to_owned(),
        ),
    }
}

/// Read a kill-session workflow's report.
#[must_use]
pub fn kill_result(report: &KillReport) -> KillResult {
    let (session_status, session_refusal, session_message) = match &report.session_outcome {
        SessionOutcome::Deleted => (TargetStatus::Done, 0, String::new()),
        SessionOutcome::AlreadyGone => (TargetStatus::AlreadyGone, 0, String::new()),
        SessionOutcome::Kept => (
            TargetStatus::Kept,
            0,
            "a member could not be removed, so the session was kept".to_owned(),
        ),
        SessionOutcome::Refused(refusal) => (
            TargetStatus::Refused,
            refusal_code(refusal),
            refusal_sentence(refusal),
        ),
        SessionOutcome::Unknown(reason) => (TargetStatus::Unknown, 0, reason.clone()),
    };
    let mut targets = vec![TargetResult {
        key: *report.session.as_bytes(),
        is_session: true,
        status: session_status,
        refusal: session_refusal,
        message: session_message.clone(),
    }];
    targets.extend(report.targets.iter().map(|(key, outcome)| {
        let (status, refusal, message) = target_of(outcome);
        TargetResult {
            key: *key.as_bytes(),
            is_session: false,
            status,
            refusal,
            message,
        }
    }));
    let members_gone = report
        .targets
        .iter()
        .filter(|(_, outcome)| outcome.is_gone())
        .count();
    let status = if report.complete() {
        Status::Committed
    } else if members_gone > 0 {
        Status::Partial
    } else if session_status == TargetStatus::Unknown
        || report
            .targets
            .iter()
            .any(|(_, outcome)| matches!(outcome, TargetOutcome::Unknown(_)))
    {
        Status::Unknown
    } else {
        Status::Refused
    };
    let message = if report.complete() {
        String::new()
    } else if session_message.is_empty() {
        "the session was not deleted".to_owned()
    } else {
        session_message
    };
    KillResult {
        status,
        targets,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::ids::{OperationId, ResourceKey, Revision, SessionKey};
    use phux_protocol::wire::frame::Committed;

    #[test]
    fn a_commit_reports_its_revision() {
        let committed = Committed::new(OperationId::new([1; 16]).unwrap(), Revision::new(4));
        let read = resolution(&OrgOutcome::Committed(Box::new(committed)));
        assert_eq!(read.status, Status::Committed);
        assert_eq!(read.revision, Some(4));
        assert!(read.message.is_empty());
    }

    #[test]
    fn a_conflict_carries_its_current_revision() {
        let read = resolution(&OrgOutcome::Refused(Refusal::Conflict {
            current: Revision::new(9),
        }));
        assert_eq!(read.status, Status::Refused);
        assert_eq!(read.refusal, 3);
        assert_eq!(read.refusal_word, "conflict");
        assert_eq!(read.conflict_revision, Some(9));
    }

    #[test]
    fn a_kill_that_kept_the_session_is_partial() {
        let session = SessionKey::new([2; 16]).unwrap();
        let report = KillReport {
            session,
            targets: vec![
                (ResourceKey::new([3; 16]).unwrap(), TargetOutcome::Removed),
                (ResourceKey::new([4; 16]).unwrap(), TargetOutcome::TimedOut),
            ],
            session_outcome: SessionOutcome::Kept,
        };
        let read = kill_result(&report);
        assert_eq!(read.status, Status::Partial);
        assert_eq!(read.targets.len(), 3);
        assert!(read.targets[0].is_session);
        assert_eq!(read.targets[0].status, TargetStatus::Kept);
        assert_eq!(read.targets[1].status, TargetStatus::Done);
        assert_eq!(read.targets[2].status, TargetStatus::TimedOut);
    }

    #[test]
    fn a_complete_kill_commits() {
        let report = KillReport {
            session: SessionKey::new([2; 16]).unwrap(),
            targets: vec![(
                ResourceKey::new([3; 16]).unwrap(),
                TargetOutcome::AlreadyGone,
            )],
            session_outcome: SessionOutcome::Deleted,
        };
        assert_eq!(kill_result(&report).status, Status::Committed);
    }
}
