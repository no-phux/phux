//! `AgentSession` verbs (ADR-0102, ADR-0103): spawn a session under a
//! Terminal parent, append to its stream, and bootstrap a consumer onto it.
//!
//! Stream-derived state ([`StreamEvidence`]) is forwarded to the parent
//! Terminal's arbiter, so existing `phux.agent/v1` consumers see it
//! unchanged.

use bytes::Bytes;
use phux_protocol::wire::frame::{CommandResult, CommandValue, ErrorCode};
use phux_resource::agent_session::AgentSessionResource as _;
use tokio::sync::oneshot;
use tracing::debug;

use crate::resource::agent_session::{AppendRejection, AppendRequest, StreamEvidence};
use crate::state::{ClientId, SharedState};

mod attach;
mod spawn;

pub(crate) use attach::attach_agent_session;
pub(crate) use spawn::spawn_agent_session;

/// Payload budget for one `BOOTSTRAP_CHUNK` when the connection negotiated
/// none. Records are at most 16 KiB, so this fits several per chunk.
const DEFAULT_AGENT_CHUNK_BYTES: usize = 64 * 1024;

/// Handle `APPEND_RESOURCE_OUTPUT` (ADR-0103 §3), replying with the stamped
/// `{"seq":…,"ts_ms":…}` header. Only a local owner connection may produce:
/// a remote peer must not forge a local agent's lifecycle.
pub(crate) async fn handle_append_resource_output(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    bytes: Bytes,
) -> CommandResult {
    if state.with(|s| s.native_agents().bindings.contains_key(terminal_id)) {
        return CommandResult::Error {
            code: ErrorCode::NotProducer,
            message: "SDK-owned streams require the reserved native controller".to_owned(),
        };
    }
    if !state.with(|s| s.client_may_produce(client_id)) {
        return CommandResult::Error {
            code: ErrorCode::NotProducer,
            message: "appending to a resource stream requires a local owner connection".to_owned(),
        };
    }
    // A non-native producer cannot masquerade as a registered SDK controller
    // merely by emitting the reserved provider_raw schema on an attached stream.
    let forged = std::str::from_utf8(&bytes).is_ok_and(|text| {
        text.lines().any(|line| {
            serde_json::from_str::<serde_json::Value>(line).is_ok_and(|record| {
                record["data"]["schema"].as_str()
                    == Some(phux_protocol::native_agent::NATIVE_SCHEMA)
            })
        })
    });
    if forged {
        return CommandResult::Error {
            code: ErrorCode::NotProducer,
            message: "native controller schema is reserved".to_owned(),
        };
    }
    match append_native_output(state, terminal_id, bytes).await {
        Ok(accepted) => CommandResult::OkWith(CommandValue::Json(format!(
            "{{\"seq\":{},\"ts_ms\":{}}}",
            accepted.first_seq, accepted.ts_ms
        ))),
        Err(error) => error,
    }
}

pub(super) async fn append_native_output(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    bytes: Bytes,
) -> Result<crate::resource::agent_session::AppendAccepted, CommandResult> {
    append_output(state, terminal_id, bytes, None).await
}

pub(super) async fn append_native_publication(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    bytes: Bytes,
    identity: crate::resource::agent_session::NativeAppendIdentity,
) -> Result<crate::resource::agent_session::AppendAccepted, CommandResult> {
    append_output(state, terminal_id, bytes, Some(identity)).await
}

/// The one validated ingress path: native authority is checked by the
/// dedicated broker before entry; generic producers are checked above.
async fn append_output(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    bytes: Bytes,
    publication: Option<crate::resource::agent_session::NativeAppendIdentity>,
) -> Result<crate::resource::agent_session::AppendAccepted, CommandResult> {
    let native_identity = publication.clone();
    let (core, handle) = state
        .with(|s| {
            s.terminal_from_wire(terminal_id).and_then(|core| {
                s.resource_handle(core)
                    .cloned()
                    .map(|handle| (core, handle))
            })
        })
        .ok_or_else(|| CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such resource: {terminal_id}"),
        })?;
    let session = handle
        .agent_session()
        .map_err(crate::runtime::commands::wrong_resource_kind)?;
    let (reply, rx) = oneshot::channel();
    session
        .append
        .try_send(AppendRequest {
            bytes,
            reply,
            publication,
        })
        .map_err(|_| CommandResult::Error {
            code: ErrorCode::Overflow,
            message: "the session's append queue is full".to_owned(),
        })?;
    let accepted = rx
        .await
        .map_err(|_| CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: "the session closed before the append landed".to_owned(),
        })?
        .map_err(append_rejection)?;
    if let Some(identity) = native_identity {
        if let Some(evidence) = accepted.evidence {
            publish_native_evidence(
                state,
                terminal_id,
                core,
                handle.parent,
                evidence,
                accepted.last_seq,
                identity.id == u64::MAX,
            )
            .await;
        }
        // Native ask inventory belongs to the committed broker projection, not
        // an append continuation that may resume after closure or supersession.
    } else {
        if let Some(evidence) = accepted.evidence {
            publish_stream_evidence(state, core, handle.parent, evidence).await;
        }
        publish_stream_ask(
            state,
            handle.parent,
            accepted.ask.clone(),
            accepted.evidence,
            false,
        );
    }
    Ok(accepted)
}

fn append_rejection(rejection: AppendRejection) -> CommandResult {
    match rejection {
        AppendRejection::Invalid(error) => CommandResult::Error {
            code: ErrorCode::RecordInvalid,
            message: error.to_string(),
        },
        AppendRejection::Overflow(message) => CommandResult::Error {
            code: ErrorCode::Overflow,
            message,
        },
        AppendRejection::Closed => CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: "the session closed before the append landed".to_owned(),
        },
    }
}

const fn evidence_state(
    evidence: StreamEvidence,
) -> (
    Option<&'static str>,
    Option<phux_protocol::wire::frame::ReportedAgentState>,
) {
    use phux_protocol::wire::frame::ReportedAgentState;
    match evidence {
        StreamEvidence::Working => (Some("working"), Some(ReportedAgentState::Working)),
        StreamEvidence::Blocked => (Some("blocked"), Some(ReportedAgentState::Blocked)),
        StreamEvidence::Done => (Some("done"), Some(ReportedAgentState::Done)),
        StreamEvidence::Retract => (None, None),
    }
}

fn claim_native_evidence(
    s: &mut crate::state::ServerState,
    agent: &phux_protocol::ids::ResourceId,
    session: phux_core::ids::ResourceId,
    seq: u64,
    closure: bool,
    word: Option<&str>,
) -> bool {
    let Some(binding) = s.native_agents_mut().bindings.get_mut(agent) else {
        return false;
    };
    if (binding.closed && !closure) || seq <= binding.evidence_seq {
        return false;
    }
    binding.evidence_seq = seq;
    if let Some(facet) = s
        .registry_mut()
        .resource_mut(session)
        .and_then(|resource| resource.agent.as_mut())
    {
        facet.state = word.map(str::to_owned);
    }
    true
}

async fn publish_native_evidence(
    state: &SharedState,
    agent: &phux_protocol::ids::ResourceId,
    session: phux_core::ids::ResourceId,
    parent: Option<phux_core::ids::ResourceId>,
    evidence: StreamEvidence,
    seq: u64,
    closure: bool,
) {
    let (word, reported) = evidence_state(evidence);
    let parent_handle =
        state.with(|s| parent.and_then(|parent| s.resource_handle(parent).cloned()));
    let Some(parent_handle) = parent_handle else {
        state.with_mut(|s| {
            claim_native_evidence(s, agent, session, seq, closure, word);
        });
        return;
    };
    // Waiting for mailbox capacity must not authorize a later stale enqueue.
    let Ok(permit) = parent_handle.control.reserve().await else {
        return;
    };
    let (reply, rx) = oneshot::channel();
    let enqueued = state.with_mut(|s| {
        if !claim_native_evidence(s, agent, session, seq, closure, word) {
            return false;
        }
        permit.send(crate::resource::ControlRequest::ReportStreamState {
            state: reported,
            reply,
        });
        true
    });
    if enqueued && let Ok(Err(reason)) = rx.await {
        debug!(%reason, "native append: parent declined the stream evidence");
    }
}

/// Feed a streamed question into the parent's ask ledger (ADR-0036).
/// Legacy turn completion withdraws asks; SDK questions outlive the turn.
fn publish_stream_ask(
    state: &SharedState,
    parent: Option<phux_core::ids::ResourceId>,
    ask: Option<crate::resource::agent_session::StreamAsk>,
    evidence: Option<StreamEvidence>,
    native_publication: bool,
) {
    let Some(parent) = parent else {
        return;
    };
    let (payload, wire_parent) = state.with_mut(|s| {
        let wire = s.intern_terminal_wire(parent);
        let payload = if let Some(ask) = ask {
            s.report_stream_ask(
                parent,
                phux_resource::agent_asked::AskedPayload {
                    id: ask.id,
                    question: ask.question,
                    suggestions: ask.suggestions,
                    elapsed_seconds: None,
                },
            )
            .emit_payload()
        } else {
            if matches!(evidence, Some(StreamEvidence::Retract))
                || (!native_publication && matches!(evidence, Some(StreamEvidence::Done)))
            {
                s.retract_agent_asked(parent, phux_resource::agent_asked::AskedSource::Stream);
            }
            None
        };
        crate::hub::metadata_mirror::publish_asked_flag(s, &wire, s.agent_is_asked(parent));
        (payload, Some(wire))
    });
    if let (Some(payload), Some(wire_parent)) = (payload, wire_parent) {
        crate::runtime::client::broadcast_event(state, Some(&wire_parent), &payload.into_event());
    }
}

/// Record the session's derived state on its own facet and feed it to the
/// parent Terminal's arbiter at the `Stream` rank (ADR-0103 §5).
async fn publish_stream_evidence(
    state: &SharedState,
    session: phux_core::ids::ResourceId,
    parent: Option<phux_core::ids::ResourceId>,
    evidence: StreamEvidence,
) {
    use phux_protocol::wire::frame::ReportedAgentState;
    let (word, reported) = match evidence {
        StreamEvidence::Working => (Some("working"), Some(ReportedAgentState::Working)),
        StreamEvidence::Blocked => (Some("blocked"), Some(ReportedAgentState::Blocked)),
        StreamEvidence::Done => (Some("done"), Some(ReportedAgentState::Done)),
        StreamEvidence::Retract => (None, None),
    };
    let parent_handle = state.with_mut(|s| {
        if let Some(facet) = s
            .registry_mut()
            .resource_mut(session)
            .and_then(|r| r.agent.as_mut())
        {
            facet.state = word.map(str::to_owned);
        }
        parent.and_then(|parent| s.resource_handle(parent).cloned())
    });
    let Some(parent_handle) = parent_handle else {
        return;
    };
    let (reply, rx) = oneshot::channel();
    let request = crate::resource::ControlRequest::ReportStreamState {
        state: reported,
        reply,
    };
    if parent_handle.control.send(request).await.is_ok() {
        // `Err` means the pane has no detector, not a producer error.
        if let Ok(Err(reason)) = rx.await {
            debug!(%reason, "APPEND_RESOURCE_OUTPUT: parent declined the stream evidence");
        }
    }
}
