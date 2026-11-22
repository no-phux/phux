use super::closure::{begin_closure, commit_closure, schedule_closure};
use super::publication::sync_ask_inventory;
use crate::state::ClientId;
use crate::state::SharedState;
use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::NativeCommand;
use phux_protocol::native_agent::NativeCommandResult;
use phux_protocol::native_agent::NativeDisposition;
use phux_protocol::native_agent::NativeIntent;
use phux_protocol::native_agent::NativeRequest;
use phux_protocol::native_agent::NativeRequestPayload;
use phux_protocol::native_agent::questions::{QuestionOperationId, RespondQuestion};
use phux_protocol::wire::frame::AgentEvent;
use phux_protocol::wire::frame::CommandResult;

/// Recheck the original caller at the actual dispatch point. A queued
/// reservation does not outlive a revoked grant, stolen lease, or question.
fn dispatch_view_valid(
    s: &crate::state::ServerState,
    agent: &ResourceId,
    command: &NativeCommand,
    requester: ClientId,
) -> bool {
    let current = s.native_agents().bindings.get(agent);
    current.is_some_and(|binding| {
        binding.view.connected
            && match &command.intent {
                NativeIntent::Interrupt { turn_id } => {
                    binding.view.turn_id.as_ref() == Some(turn_id)
                }
                NativeIntent::Answer { question_id, .. } => binding
                    .view
                    .questions
                    .iter()
                    .any(|question| &question.id == question_id && human(s, requester)),
                NativeIntent::AnswerExact { question, response } => {
                    human(s, requester)
                        && binding
                            .validate_current_response(&question.id, response, Some(question))
                            .is_ok()
                }
                NativeIntent::Prompt { .. } => true,
            }
    })
}

fn human(s: &crate::state::ServerState, requester: ClientId) -> bool {
    s.connection_grant(requester)
        .is_some_and(|grant| grant.kind() == phux_core::actor::PrincipalKind::Human)
}

pub(super) fn refuse_undeliverable(s: &mut crate::state::ServerState, agent: &ResourceId) {
    let Some(binding) = s.native_agents().bindings.get(agent) else {
        return;
    };
    let parent = binding.parent.clone();
    let Some(core_parent) = s.idspace.resource_from_wire(&parent) else {
        return;
    };
    let candidates: Vec<_> = binding
        .pending
        .iter()
        .filter_map(|id| binding.operations.get(id))
        .map(|operation| (operation.command.clone(), ClientId(operation.requester)))
        .collect();
    for (command, requester) in candidates {
        let valid = dispatch_view_valid(s, agent, &command, requester);
        let wire_command = request_for_dispatch(agent, &command)
            .map(|request| phux_protocol::wire::frame::Command::NativeAgent { request });
        if valid
            && wire_command.as_ref().is_ok_and(|wire_command| {
                dispatch_input_allowed(s, requester, core_parent, &parent, agent, wire_command)
            })
        {
            continue;
        }
        let result = NativeCommandResult {
            operation_id: command.operation_id,
            disposition: NativeDisposition::Refused,
            detail: Some(match wire_command {
                Err(message) => message,
                Ok(_) => {
                    "caller authority, lease, source, turn, or question changed before dispatch"
                        .into()
                }
            }),
        };
        if let Some(binding) = s.native_agents_mut().bindings.get_mut(agent) {
            binding.pending.retain(|id| *id != command.operation_id);
            if let Some(operation) = binding.operations.get_mut(&command.operation_id) {
                operation.result = result.clone();
            }
            if let Some(question_id) = command.intent.question_id() {
                binding.answered.remove(question_id);
            }
        }
        let event = AgentEvent::NativeAgentResult {
            agent_session_id: agent.clone(),
            operation_id: result.operation_id,
            disposition: result.disposition,
            detail: result.detail,
        };
        s.record_and_fanout(
            crate::state::EventRecord::new(Some(agent.clone()), event)
                .with_parent(Some(parent.clone())),
        );
    }
}

fn dispatch_input_allowed(
    s: &crate::state::ServerState,
    requester: ClientId,
    core_parent: phux_core::ids::ResourceId,
    parent: &ResourceId,
    agent: &ResourceId,
    wire_command: &phux_protocol::wire::frame::Command,
) -> bool {
    !s.leases().blocked(core_parent, requester)
        && !s.clients().is_viewer(requester, parent)
        && !s.clients().is_viewer(requester, agent)
        && matches!(
            crate::policy::authorize_command(s, requester, wire_command),
            Ok(crate::policy::Admission::Run)
        )
}

fn request_for_dispatch(
    agent: &ResourceId,
    command: &NativeCommand,
) -> Result<NativeRequestPayload, String> {
    let request = match &command.intent {
        NativeIntent::AnswerExact { question, response } => NativeRequest::RespondQuestion {
            answer: RespondQuestion {
                operation_id: QuestionOperationId::new(command.operation_id)
                    .ok_or_else(|| "operation identity must be nonzero".to_owned())?,
                question: question.clone(),
                response: response.clone(),
            },
        },
        _ => NativeRequest::Command {
            agent_session_id: agent.clone(),
            command: command.clone(),
        },
    };
    NativeRequestPayload::new(request)
}

/// A real executing-host/stream exit is source closure, not an interrupt.
/// Publish unavailable controls while the readable stream is still alive.
pub(crate) async fn source_exited(state: &SharedState, resource: phux_core::ids::ResourceId) {
    let agents = state.with_mut(|s| {
        let Some(wire) = s.idspace.resource_wire(resource).cloned() else {
            return Vec::new();
        };
        let agents: Vec<_> = s
            .native_agents()
            .bindings
            .values()
            .filter(|binding| {
                (binding.parent == wire || binding.agent == wire) && !binding.closure_committed
            })
            .map(|binding| binding.agent.clone())
            .collect();
        for agent in &agents {
            begin_closure(s, agent);
        }
        agents
    });
    for agent in agents {
        schedule_closure(state, &agent);
        sync_ask_inventory(state, &agent);
        if let CommandResult::Error { message, .. } = commit_closure(state, &agent).await {
            tracing::warn!(?agent, %message, "native closure pending durable append");
        }
    }
}
