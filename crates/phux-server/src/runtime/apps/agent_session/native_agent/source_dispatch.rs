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
                NativeIntent::Answer { question_id, .. } => {
                    binding.view.questions.iter().any(|question| {
                        &question.id == question_id
                            && s.connection_grant(requester).is_some_and(|grant| {
                                grant.kind() == phux_core::actor::PrincipalKind::Human
                            })
                    })
                }
                NativeIntent::Prompt { .. } => true,
            }
    })
}

pub(super) fn refuse_undeliverable(s: &mut crate::state::ServerState, agent: &ResourceId) {
    let Some(binding) = s.native_agents().bindings.get(agent) else {
        return;
    };
    let parent = binding.parent.clone();
    let Some(core_parent) = s.resource_from_wire(&parent) else {
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
        let wire_command = NativeRequestPayload::new(NativeRequest::Command {
            agent_session_id: agent.clone(),
            command: command.clone(),
        })
        .map(|request| phux_protocol::wire::frame::Command::NativeAgent { request });
        if valid
            && !s.input_blocked(core_parent, requester)
            && !s.is_viewer(requester, &parent)
            && wire_command.as_ref().is_ok_and(|wire_command| {
                matches!(
                    crate::policy::authorize_command(s, requester, wire_command),
                    Ok(crate::policy::Admission::Run)
                )
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
            if let NativeIntent::Answer { question_id, .. } = command.intent {
                binding.answered.remove(&question_id);
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

/// A real executing-host/stream exit is source closure, not an interrupt.
/// Publish unavailable controls while the readable stream is still alive.
pub(crate) async fn source_exited(state: &SharedState, resource: phux_core::ids::ResourceId) {
    let agents = state.with_mut(|s| {
        let Some(wire) = s.resource_wire_of(resource) else {
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
