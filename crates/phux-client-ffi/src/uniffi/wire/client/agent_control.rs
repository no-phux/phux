//! Agent controls and agent records, projected for bindings, and the
//! server's held approvals (ADR-0128) with the decision a phone sends.
use phux_client_runtime::approvals::{Approval, Decision, DecisionOutcome};
use phux_protocol::ids::ApprovalId;

use super::*;

/// One action the server holds for a person: its `phux.approval/v1/<id>`
/// record.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct WireApproval {
    /// 32 lowercase hex digits; what an `approve` push names as `question`.
    pub id: String,
    /// Who asked: the name it announced, its credential, or `client <n>`.
    pub requester: String,
    /// The held command's catalog name, e.g. `KILL_RESOURCE`.
    pub method: String,
    /// The signal, for a held `SIGNAL_TERMINAL`.
    pub signal: Option<String>,
    /// What it acts on, e.g. `terminal:3` or `session:<key>`.
    pub subjects: Vec<String>,
    pub requested_at_ms: u64,
    /// Refused as expired unless decided by then.
    pub expires_at_ms: u64,
}

impl From<Approval> for WireApproval {
    fn from(approval: Approval) -> Self {
        let who = approval.requester;
        Self {
            id: approval.id,
            requester: who
                .client_name
                .or(who.credential_id)
                .unwrap_or_else(|| format!("client {}", who.client)),
            method: approval.method,
            signal: approval.signal,
            subjects: approval.subjects,
            requested_at_ms: approval.requested_at_ms,
            expires_at_ms: approval.expires_at_ms,
        }
    }
}

#[uniffi::export]
impl RemoteClient {
    /// Follow the server's held approvals on this connection and every
    /// later one: `ApprovalsChanged` whenever `approvals()` changes. Call
    /// it once connected; a no-op offline.
    pub fn watch_approvals(&self) {
        if let Some(client) = self.runtime_client() {
            client.watch_approvals();
        }
    }

    /// The pending approvals, oldest first. Empty unless watching, or when
    /// the server does not hold actions.
    pub fn approvals(&self) -> Vec<WireApproval> {
        self.runtime_client()
            .map(|client| client.approvals().into_iter().map(Into::into).collect())
            .unwrap_or_default()
    }

    /// Approve or deny held action `id`. The outcome arrives once as
    /// `ApprovalDecided`; it is never retried. `false` when nothing was
    /// sent: offline, a malformed id, a server without approvals, or a
    /// decision on `id` already in flight.
    pub fn decide_approval(&self, id: String, approve: bool) -> bool {
        let (Some(client), Some(id)) = (self.runtime_client(), ApprovalId::parse(&id)) else {
            return false;
        };
        let decision = if approve {
            Decision::Approve
        } else {
            Decision::Deny
        };
        client.decide_approval(id, decision)
    }
}

/// Project human-facing agent controls, returning any unhandled event unchanged.
pub(super) fn project_agent_control(event: Event, projected: &mut Vec<WireEvent>) -> Option<Event> {
    let event = match event {
        Event::AgentAsked {
            terminal_id,
            question_id,
            text,
            suggestions,
            waiting_seconds,
        } => WireEvent::AgentAsked {
            terminal_id: id::encode(&terminal_id),
            question_id,
            text,
            suggestions,
            waiting_seconds,
        },
        Event::AgentAskedState { terminal_id, asked } => WireEvent::AgentAskedState {
            terminal_id: id::encode(&terminal_id),
            asked,
        },
        Event::BackgroundWork {
            terminal_id,
            running,
        } => WireEvent::BackgroundWork {
            terminal_id: id::encode(&terminal_id),
            running,
        },
        Event::NativeAgentResult {
            agent_session_id,
            operation_id,
            disposition,
            detail,
        } => WireEvent::NativeAgentResult {
            agent_session_id: id::encode(&agent_session_id),
            operation_id,
            disposition: match disposition {
                phux_protocol::native_agent::NativeDisposition::Admitted => "admitted",
                phux_protocol::native_agent::NativeDisposition::Queued => "queued",
                phux_protocol::native_agent::NativeDisposition::Completed => "completed",
                phux_protocol::native_agent::NativeDisposition::Refused => "refused",
                phux_protocol::native_agent::NativeDisposition::Unknown => "unknown",
            }
            .into(),
            detail,
        },
        Event::AgentPublication {
            publication,
            session,
        } => {
            let batch = crate::projection::agent_records::project(&publication)?;
            let kind = match batch.mode {
                phux_client_core::session::agent_stream::AgentRecordMode::Replace => {
                    AgentRecordsKind::Retained
                }
                phux_client_core::session::agent_stream::AgentRecordMode::Append => {
                    AgentRecordsKind::Live
                }
            };
            let id = &publication.resource_id;
            agent_records(id, session, kind, batch.seq, batch.jsonl)
        }
        Event::AgentSessionClosed {
            terminal_id,
            session,
            ..
        } => agent_records(
            &terminal_id,
            session,
            AgentRecordsKind::Closed,
            0,
            String::new(),
        ),
        Event::ApprovalsChanged => WireEvent::ApprovalsChanged,
        Event::ApprovalDecision { id, outcome, .. } => {
            let (outcome, message) = match outcome {
                DecisionOutcome::Decided => ("decided", None),
                DecisionOutcome::Stale => ("stale", None),
                DecisionOutcome::Refused { message, .. } => ("refused", Some(message)),
                DecisionOutcome::Unconfirmed => ("unconfirmed", None),
            };
            WireEvent::ApprovalDecided {
                id: id.to_string(),
                outcome: outcome.into(),
                message,
            }
        }
        event => return Some(event),
    };
    projected.push(event);
    None
}

/// Lower one agent session's records, or its end, to the foreign event.
fn agent_records(
    terminal_id: &ResourceId,
    session: phux_client_runtime::control::AgentSessionInfo,
    kind: AgentRecordsKind,
    seq: u64,
    jsonl: String,
) -> WireEvent {
    WireEvent::AgentRecords {
        agent_session_id: id::encode(terminal_id),
        parent_terminal_id: session.parent.as_ref().map(id::encode),
        provider: session.provider,
        native_id: session.native_id,
        kind,
        seq,
        jsonl,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approvals_and_decisions_lower_for_bindings() {
        let record = br#"{"schema_version":1,"id":"0123456789abcdef0123456789abcdef",
            "requester":{"client":4,"credential_id":"ci-bot"},"method":"SIGNAL_TERMINAL",
            "signal":"kill","subjects":["terminal:3"],"requested_at_ms":1,"expires_at_ms":2}"#;
        let approval = WireApproval::from(Approval::parse(record).expect("record"));
        assert_eq!(approval.requester, "ci-bot");
        assert_eq!(approval.signal.as_deref(), Some("kill"));
        assert_eq!(approval.subjects, ["terminal:3"]);

        let id = ApprovalId::parse(&approval.id).expect("id");
        let mut projected = Vec::new();
        for event in [
            Event::ApprovalsChanged,
            Event::ApprovalDecision {
                id,
                decision: Decision::Deny,
                outcome: DecisionOutcome::Refused {
                    code: phux_protocol::wire::frame::ErrorCode::PermissionDenied,
                    message: "held".into(),
                },
            },
        ] {
            assert!(project_agent_control(event, &mut projected).is_none());
        }
        assert!(matches!(projected[0], WireEvent::ApprovalsChanged));
        let WireEvent::ApprovalDecided {
            id,
            outcome,
            message,
        } = &projected[1]
        else {
            panic!("{projected:?}");
        };
        assert_eq!(
            (id.as_str(), outcome.as_str(), message.as_deref()),
            (approval.id.as_str(), "refused", Some("held"))
        );
    }
}
