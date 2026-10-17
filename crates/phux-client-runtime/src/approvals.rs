//! Server-held approvals (ADR-0128, `docs/spec/L3.md` §3.10): the record a
//! server publishes for each held action, and the decision a client sends.
//!
//! The pending set is the server-owned `phux.approval/v1/<id>` records under
//! `Global`. A decision is a `SET_METADATA` of `phux.approval.decide/v1/<id>`
//! with `approve` or `deny`, which the server intercepts and never stores.
//! `SET_METADATA` has no reply, so a decision is confirmed by reading its
//! record back on the same connection: the server handles one connection's
//! frames in order, so a refused decision's correlated `ERROR` arrives before
//! the read's answer.
//!
//! These types are the one parse of the record. The one-shot verbs
//! (`phux approvals`, MCP) and the control plane's live projection
//! ([`crate::control::ControlPlane::watch_approvals`]) share them.

use phux_protocol::wire::frame::{APPROVAL_APPROVE, APPROVAL_DENY, ErrorCode};
use serde::{Deserialize, Serialize};

/// One action awaiting a decision: its `phux.approval/v1/<id>` record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    /// The approval id, 32 lowercase hex digits.
    pub id: String,
    /// The connection whose command is held.
    pub requester: Requester,
    /// The held command's catalog name, e.g. `KILL_RESOURCE`.
    pub method: String,
    /// The signal, for a held `SIGNAL_TERMINAL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    /// The subjects it names, in the registry selector grammar.
    #[serde(default)]
    pub subjects: Vec<String>,
    /// When it was held, Unix milliseconds.
    pub requested_at_ms: u64,
    /// When it expires unless decided, Unix milliseconds.
    pub expires_at_ms: u64,
}

impl Approval {
    /// Parse a `phux.approval/v1/<id>` value.
    ///
    /// # Errors
    ///
    /// The value is not the record's JSON object.
    pub fn parse(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

/// Who asked: the requester's connection as the journal names an actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requester {
    /// The connection's wire client id.
    pub client: u32,
    /// The workload credential it authenticated with, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    /// The name it announced in `HELLO`, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
}

/// A decision on one held action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Release the held command: the server runs it once, as the requester.
    Approve,
    /// Refuse it: the requester gets `PERMISSION_DENIED`.
    Deny,
}

impl Decision {
    /// The `phux.approval.decide/v1/<id>` value that sends it.
    #[must_use]
    pub const fn value(self) -> &'static [u8] {
        match self {
            Self::Approve => APPROVAL_APPROVE,
            Self::Deny => APPROVAL_DENY,
        }
    }

    /// The verb that sends it, `approve` or `deny`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Deny => "deny",
        }
    }
}

/// How one decision ended, read from the decision's correlated refusal (if
/// any) and the record read that follows it on the same connection.
///
/// None of these is retried: a decision releases a destructive action at
/// most once, and only a person decides again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionOutcome {
    /// The server took the decision: the record is gone.
    Decided,
    /// The server refused the decision and the record is gone: the approval
    /// was already decided elsewhere, expired, or was withdrawn first.
    Stale,
    /// The server refused the decision and the approval is still pending
    /// (or its record could not be read): typically no un-held `SIGNAL` on
    /// the held action's subject.
    Refused {
        /// The refusal's code.
        code: ErrorCode,
        /// The refusal's message.
        message: String,
    },
    /// No confirmation arrived: the connection ended first, or the record
    /// was still listed after an unrefused decision. Re-read the pending set
    /// before deciding again.
    Unconfirmed,
}

impl DecisionOutcome {
    /// Classify a decision from its refusal and the confirming read.
    ///
    /// `still_pending` is `Some(true)` when the record was read back,
    /// `Some(false)` when it was absent, and `None` when the read itself was
    /// refused (a decider without `OBSERVE` on `Global`).
    #[must_use]
    pub fn classify(refusal: Option<(ErrorCode, String)>, still_pending: Option<bool>) -> Self {
        match (refusal, still_pending) {
            (None, Some(false) | None) => Self::Decided,
            (None, Some(true)) => Self::Unconfirmed,
            (Some(_), Some(false)) => Self::Stale,
            (Some((code, message)), Some(true) | None) => Self::Refused { code, message },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_parses_with_and_without_its_optional_fields() {
        let full = br#"{"schema_version":1,"id":"0123456789abcdef0123456789abcdef","requester":{"client":3,"credential_id":"sha256:x","client_name":"agent"},"method":"SIGNAL_TERMINAL","signal":"kill","subjects":["terminal:4"],"requested_at_ms":1,"expires_at_ms":120001}"#;
        let approval = Approval::parse(full).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(approval.signal.as_deref(), Some("kill"));
        assert_eq!(approval.requester.client, 3);
        let bare = br#"{"id":"0123456789abcdef0123456789abcdef","requester":{"client":3},"method":"UPGRADE","requested_at_ms":1,"expires_at_ms":2}"#;
        let approval = Approval::parse(bare).unwrap_or_else(|err| panic!("{err}"));
        assert!(approval.subjects.is_empty());
        assert_eq!(approval.requester.credential_id, None);
    }

    #[test]
    fn a_decision_is_classified_from_its_refusal_and_the_read_back() {
        let denied = || Some((ErrorCode::PermissionDenied, "permission denied".to_owned()));
        assert_eq!(
            DecisionOutcome::classify(None, Some(false)),
            DecisionOutcome::Decided
        );
        assert_eq!(
            DecisionOutcome::classify(None, None),
            DecisionOutcome::Decided,
            "an unreadable record says nothing against an unrefused decision"
        );
        assert_eq!(
            DecisionOutcome::classify(None, Some(true)),
            DecisionOutcome::Unconfirmed
        );
        assert_eq!(
            DecisionOutcome::classify(denied(), Some(false)),
            DecisionOutcome::Stale
        );
        assert!(matches!(
            DecisionOutcome::classify(denied(), Some(true)),
            DecisionOutcome::Refused {
                code: ErrorCode::PermissionDenied,
                ..
            }
        ));
        assert!(matches!(
            DecisionOutcome::classify(denied(), None),
            DecisionOutcome::Refused { .. }
        ));
        assert_eq!(Decision::Approve.value(), APPROVAL_APPROVE);
        assert_eq!(Decision::Deny.as_str(), "deny");
    }
}
