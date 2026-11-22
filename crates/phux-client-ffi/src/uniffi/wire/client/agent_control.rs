//! Agent controls and agent records, projected for bindings.
use super::*;

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
