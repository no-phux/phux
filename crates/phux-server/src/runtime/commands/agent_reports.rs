//! Agent state, terminal event, and asked reports.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;

/// Feed integration-hook lifecycle evidence into a pane (ADR-0085): as a
/// synthesized record on a live `AgentSession` child's stream, else straight
/// into the detector (ADR-0103 decision 6).
pub(crate) async fn handle_report_agent_state(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    reported: phux_protocol::wire::frame::ReportedAgentState,
) -> CommandResult {
    if state.with(|s| {
        s.resource_from_wire(terminal_id)
            .is_some_and(|core| s.native_owned_terminal(core))
    }) {
        return CommandResult::Error {
            code: ErrorCode::NotProducer,
            message: "SDK-owned lifecycle requires its native controller".to_owned(),
        };
    }
    // One borrow, so a `session_end` cannot land between the two reads.
    let (resolved, live_session) = state.with(|server| {
        let resolved = server.resolve_resource(terminal_id).into_owned();
        let live_session = matches!(resolved, ResolvedOwned::Local(_))
            && server.resource_has_live_session(terminal_id);
        (resolved, live_session)
    });
    // The report addresses the instrumented Terminal, never the child stream.
    let (_, handle) = match local_terminal(resolved, terminal_id, "REPORT_AGENT_STATE") {
        Ok(local) => local,
        Err(refusal) => return refusal,
    };
    let (reply, result) = oneshot::channel();
    let request = if live_session {
        ControlRequest::SynthesizeAgentStateRecord {
            state: reported,
            reply,
        }
    } else {
        ControlRequest::ReportAgentState {
            state: reported,
            reply,
        }
    };
    if handle.control.send(request).await.is_err() {
        return CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "pane actor unavailable for REPORT_AGENT_STATE".to_owned(),
        };
    }
    actor_reply(
        result.await,
        ErrorCode::InvalidCommand,
        "REPORT_AGENT_STATE",
    )
}

/// Handle `SUBSCRIBE_RESOURCE_EVENTS`: a Terminal-scoped `SUBSCRIBE_EVENTS`
/// with a type filter in the same registry (ADR-0123); a repeat replaces the
/// filter. Registered before `Ok`, so no later event is missed.
pub(crate) fn handle_subscribe_terminal_events(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    event_types: Vec<phux_protocol::wire::frame::ResourceEventType>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> CommandResult {
    let filter = crate::state::EventFilter::of(event_types);
    let registered = state.with_mut(|s| {
        s.resource_from_wire(terminal_id)?;
        s.subscribe_resource_events(client_id, terminal_id.clone(), filter, out_tx.clone());
        Some(())
    });
    if registered.is_none() {
        return terminal_not_found(terminal_id);
    }
    super::super::client::ensure_event_pump(state, client_id);
    debug!(
        ?client_id,
        ?terminal_id,
        "SUBSCRIBE_RESOURCE_EVENTS: subscriber registered"
    );
    CommandResult::Ok
}

pub(crate) fn handle_report_asked(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    id: String,
    question: String,
    suggestions: Vec<String>,
    elapsed_seconds: Option<u64>,
) -> CommandResult {
    let resolved = state.with(|s| s.resolve_resource(terminal_id).into_owned());
    let terminal = match local_terminal(resolved, terminal_id, "REPORT_ASKED") {
        Ok((terminal, _)) => terminal,
        Err(refusal) => return refusal,
    };
    if state.with(|s| s.native_owned_terminal(terminal)) {
        return CommandResult::Error {
            code: ErrorCode::NotProducer,
            message: "SDK-owned questions require its native controller".to_owned(),
        };
    }
    if let Some(message) = validate_asked_payload(&id, &question, &suggestions) {
        return CommandResult::Error {
            code: ErrorCode::InvalidCommand,
            message,
        };
    }
    let payload = AskedPayload {
        id,
        question,
        suggestions,
        elapsed_seconds,
    };
    let transition = state.with_mut(|s| {
        let transition = s.report_agent_asked(terminal, AskedSource::Hook, payload);
        crate::hub::metadata_mirror::publish_asked_flag(s, terminal_id, s.agent_is_asked(terminal));
        transition
    });
    if let Some(payload) = transition.emit_payload() {
        super::super::client::broadcast_event(state, Some(terminal_id), &payload.into_event());
    }
    CommandResult::Ok
}

fn validate_asked_payload(id: &str, question: &str, suggestions: &[String]) -> Option<String> {
    const MAX_ID_BYTES: usize = 128;
    const MAX_QUESTION_BYTES: usize = 4096;
    const MAX_SUGGESTIONS: usize = 16;
    const MAX_SUGGESTION_BYTES: usize = 512;

    if question.trim().is_empty() {
        return Some("asked question must not be empty".to_owned());
    }
    if id.len() > MAX_ID_BYTES {
        return Some(format!("asked id exceeds {MAX_ID_BYTES} bytes"));
    }
    if question.len() > MAX_QUESTION_BYTES {
        return Some(format!("asked question exceeds {MAX_QUESTION_BYTES} bytes"));
    }
    if suggestions.len() > MAX_SUGGESTIONS {
        return Some(format!(
            "asked suggestions exceed {MAX_SUGGESTIONS} entries"
        ));
    }
    for suggestion in suggestions {
        if suggestion.trim().is_empty() {
            return Some("asked suggestions must not be empty".to_owned());
        }
        if suggestion.len() > MAX_SUGGESTION_BYTES {
            return Some(format!(
                "asked suggestion exceeds {MAX_SUGGESTION_BYTES} bytes"
            ));
        }
    }
    None
}
