//! Encoding an `AgentEvent`: its tag, then its length-prefixed body.

use super::{
    AgentEvent, ApprovalOutcome, BytesMut, DecodeError, Decoder, EVENT_TAG_APPROVAL_DECIDED,
    EVENT_TAG_APPROVAL_REQUESTED, EVENT_TAG_ASKED, EVENT_TAG_BELL, EVENT_TAG_COMMAND_FINISHED,
    EVENT_TAG_COMMAND_STARTED, EVENT_TAG_CWD_CHANGED, EVENT_TAG_DIRTY, EVENT_TAG_IDLE,
    EVENT_TAG_INPUT_ACTIVITY, EVENT_TAG_JOURNAL_GAP, EVENT_TAG_RESOURCE_CLOSED,
    EVENT_TAG_RESOURCE_SPAWNED, EVENT_TAG_SOURCE_GAP, EVENT_TAG_TERMINAL_CONTROL,
    EVENT_TAG_TITLE_CHANGED, Encoder, encode_optional_client_id, encode_optional_i32,
    encode_terminal_id, field,
};

#[allow(
    clippy::too_many_lines,
    reason = "one arm per AgentEvent wire tag; the dispatch is a flat encode table"
)]
pub(in crate::wire) fn encode_agent_event(event: &AgentEvent, enc: &mut Encoder<'_>) {
    let mut body = BytesMut::new();
    let tag = {
        let mut body_enc = Encoder::new(&mut body);
        match event {
            AgentEvent::NativeAgentResult {
                agent_session_id,
                operation_id,
                disposition,
                detail,
            } => {
                encode_terminal_id(agent_session_id, &mut body_enc);
                body_enc.write_u64_be(*operation_id);
                body_enc.write_u8(disposition.to_u8());
                // A presence byte, so a newer peer can append after it.
                body_enc.write_option(detail.as_deref(), Encoder::write_str);
                crate::wire::frame::EVENT_TAG_NATIVE_AGENT_RESULT
            }
            AgentEvent::CommandStarted => EVENT_TAG_COMMAND_STARTED,
            AgentEvent::CommandFinished { exit_code } => {
                encode_optional_i32(*exit_code, &mut body_enc);
                EVENT_TAG_COMMAND_FINISHED
            }
            AgentEvent::TitleChanged { title } => {
                body_enc.write_str(title);
                EVENT_TAG_TITLE_CHANGED
            }
            AgentEvent::Bell => EVENT_TAG_BELL,
            AgentEvent::ResourceSpawned { kind, parent } => {
                if !kind.is_terminal() {
                    body_enc.write_field(field::event_pane_spawned::KIND, &[kind.as_wire()]);
                }
                if let Some(parent) = parent {
                    body_enc.write_field_with(field::event_pane_spawned::PARENT, |e| {
                        encode_terminal_id(parent, e);
                    });
                }
                EVENT_TAG_RESOURCE_SPAWNED
            }
            AgentEvent::ResourceClosed { exit_status } => {
                encode_optional_i32(*exit_status, &mut body_enc);
                EVENT_TAG_RESOURCE_CLOSED
            }
            AgentEvent::Dirty => EVENT_TAG_DIRTY,
            AgentEvent::Idle => EVENT_TAG_IDLE,
            AgentEvent::TerminalControl {
                lifecycle,
                exit_status,
                input_holder,
                action,
                actor,
            } => {
                body_enc.write_u8(lifecycle.to_u8());
                encode_optional_i32(*exit_status, &mut body_enc);
                encode_optional_client_id(*input_holder, &mut body_enc);
                body_enc.write_u8(action.to_u8());
                encode_optional_client_id(*actor, &mut body_enc);
                EVENT_TAG_TERMINAL_CONTROL
            }
            AgentEvent::Asked {
                id,
                question,
                suggestions,
                elapsed_seconds,
            } => {
                encode_asked_fields(id, question, suggestions, *elapsed_seconds, &mut body_enc);
                EVENT_TAG_ASKED
            }
            AgentEvent::CwdChanged { cwd } => {
                body_enc.write_str(cwd);
                EVENT_TAG_CWD_CHANGED
            }
            AgentEvent::JournalGap {
                first_missing,
                last_missing,
            } => {
                write_u64_field(
                    &mut body_enc,
                    field::event_journal_gap::FIRST_MISSING,
                    *first_missing,
                );
                write_u64_field(
                    &mut body_enc,
                    field::event_journal_gap::LAST_MISSING,
                    *last_missing,
                );
                EVENT_TAG_JOURNAL_GAP
            }
            AgentEvent::SourceGap { dropped } => {
                write_u64_field(&mut body_enc, field::event_source_gap::DROPPED, *dropped);
                EVENT_TAG_SOURCE_GAP
            }
            AgentEvent::ApprovalRequested { id } => {
                write_approval_id(id, &mut body_enc);
                EVENT_TAG_APPROVAL_REQUESTED
            }
            AgentEvent::ApprovalDecided { id, outcome } => {
                write_approval_id(id, &mut body_enc);
                body_enc.write_u8(outcome.to_u8());
                EVENT_TAG_APPROVAL_DECIDED
            }
            AgentEvent::InputActivity { origin } => {
                body_enc.write_str(origin);
                EVENT_TAG_INPUT_ACTIVITY
            }
            org @ (AgentEvent::SessionCreated { .. }
            | AgentEvent::SessionRenamed { .. }
            | AgentEvent::SessionProjectSet { .. }
            | AgentEvent::SessionDeleted { .. }
            | AgentEvent::SessionLifetimeChanged { .. }
            | AgentEvent::MembershipChanged { .. }
            | AgentEvent::ResourceCreated { .. }
            | AgentEvent::ResourceRemoved { .. }
            | AgentEvent::ActivationChanged { .. }
            | AgentEvent::ArrangementChanged { .. }
            | AgentEvent::LeaseChanged { .. }
            | AgentEvent::IntakeChanged { .. }
            | AgentEvent::ResourceRenamed { .. }) => {
                crate::wire::frame::org::encode_org_event(org, &mut body_enc).unwrap_or_default()
            }
            // Re-emit an unknown event's body verbatim so relays are lossless.
            AgentEvent::Unknown { tag, .. } => *tag,
        }
    };
    if let AgentEvent::Unknown { body: raw, .. } = event {
        body.extend_from_slice(raw);
    }
    enc.write_u8(tag);
    enc.write_bytes(&body);
}

/// Write an approval id as its 16 raw bytes (ADR-0128).
pub(super) fn write_approval_id(id: &crate::ids::ApprovalId, enc: &mut Encoder<'_>) {
    for byte in id.as_bytes() {
        enc.write_u8(*byte);
    }
}

/// Decode an approval event body (16 id bytes, then a decision's outcome);
/// `Ok(None)` for a zero id or unknown outcome.
pub(super) fn decode_approval_event(
    tag: u8,
    dec: &mut Decoder<'_>,
) -> Result<Option<AgentEvent>, DecodeError> {
    let mut bytes = [0; 16];
    for byte in &mut bytes {
        *byte = dec.read_u8()?;
    }
    let id = crate::ids::ApprovalId::new(bytes);
    if tag == EVENT_TAG_APPROVAL_REQUESTED {
        return Ok(id.map(|id| AgentEvent::ApprovalRequested { id }));
    }
    let outcome = ApprovalOutcome::from_u8(dec.read_u8()?);
    Ok(id
        .zip(outcome)
        .map(|(id, outcome)| AgentEvent::ApprovalDecided { id, outcome }))
}

/// Write one field-tagged `u64` inside an event body.
pub(super) fn write_u64_field(enc: &mut Encoder<'_>, field_id: u32, value: u64) {
    enc.write_field_with(field_id, |e| e.write_u64_be(value));
}

pub(super) fn encode_asked_fields(
    id: &str,
    question: &str,
    suggestions: &[String],
    elapsed_seconds: Option<u64>,
    enc: &mut Encoder<'_>,
) {
    // Shared by `Command::ReportAsked` and `AgentEvent::Asked` so they cannot
    // drift.
    enc.write_field(field::event_asked::ID, id.as_bytes());
    enc.write_field(field::event_asked::QUESTION, question.as_bytes());
    for suggestion in suggestions {
        enc.write_field(field::event_asked::SUGGESTION, suggestion.as_bytes());
    }
    if let Some(secs) = elapsed_seconds {
        enc.write_field_with(field::event_asked::ELAPSED_SECONDS, |e| {
            e.write_u64_be(secs);
        });
    }
}
