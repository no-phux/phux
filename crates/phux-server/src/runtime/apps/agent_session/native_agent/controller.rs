use super::closure::close;
use super::integrations;
use super::launch::spawn;
use super::publication::publish;
use super::source_dispatch::refuse_undeliverable;
use crate::runtime::commands::CommandContext;
use crate::state::ClientId;
use crate::state::SharedState;
use crate::state::native_agent::NativeBinding;
use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::NativeCommand;
use phux_protocol::native_agent::NativeCommandResult;
use phux_protocol::native_agent::NativeDisposition;
use phux_protocol::native_agent::NativeIntent;
use phux_protocol::native_agent::NativeRequest;
use phux_protocol::wire::frame::AgentEvent;
use phux_protocol::wire::frame::CommandResult;
use phux_protocol::wire::frame::CommandValue;
use phux_protocol::wire::frame::ErrorCode;
use serde::Serialize;
use subtle::ConstantTimeEq as _;

use phux_resource::agent_session::AgentSessionResource as _;
pub(super) fn refusal(message: impl Into<String>) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InvalidCommand,
        message: message.into(),
    }
}
pub(super) fn denied(message: impl Into<String>) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::PermissionDenied,
        message: message.into(),
    }
}
pub(super) fn closed(message: impl Into<String>) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::TerminalNotFound,
        message: message.into(),
    }
}
pub(super) fn json<T: Serialize>(value: &T) -> CommandResult {
    match serde_json::to_string(value) {
        Ok(value) => CommandResult::OkWith(CommandValue::Json(value)),
        Err(error) => refusal(error.to_string()),
    }
}
pub(super) fn authenticated_host_binding<'a>(
    s: &'a mut crate::state::ServerState,
    client: ClientId,
    agent: &ResourceId,
    activation: &str,
    proof: &str,
    generation: Option<u64>,
) -> Result<&'a mut NativeBinding, CommandResult> {
    let (credential, pid) = s
        .native_host_identity(client)
        .ok_or_else(|| denied("native controller requires its local delegated host identity"))?;
    verify_guardian_process(s, agent, pid)?;
    let binding = s
        .native_agents_mut()
        .bindings
        .get_mut(agent)
        .ok_or_else(|| closed("no reserved native binding"))?;
    if !binding_identity_matches(binding, activation, proof, &credential, pid) {
        return Err(denied(
            "native controller identity does not match the reservation",
        ));
    }
    if generation.is_some_and(|generation| generation != binding.generation) {
        return Err(CommandResult::Error {
            code: ErrorCode::PreconditionFailed,
            message: "native controller generation is fenced".into(),
        });
    }
    binding.host_pid = Some(pid);
    Ok(binding)
}
fn verify_guardian_process(
    s: &crate::state::ServerState,
    agent: &ResourceId,
    pid: u32,
) -> Result<(), CommandResult> {
    let core = s
        .resource_from_wire(agent)
        .ok_or_else(|| closed("native agent session is gone"))?;
    if s.resource_handle(core).is_none() {
        return Err(closed("native agent session is closed"));
    }
    let parent = s
        .resource_parent(core)
        .ok_or_else(|| denied("native agent lacks its reserved guardian"))?;
    let guardian = s
        .registry()
        .record(parent)
        .and_then(|record| record.activation.as_ref())
        .filter(|activation| activation.state.is_current())
        .and_then(|activation| activation.host_proc)
        .ok_or_else(|| closed("native guardian has no live process activation"))?;
    if u32::try_from(guardian.pid).ok() != Some(pid) {
        return Err(denied(
            "only the actual guardian process may bind its controller",
        ));
    }
    Ok(())
}

fn binding_identity_matches(
    binding: &NativeBinding,
    activation: &str,
    proof: &str,
    credential: &str,
    pid: u32,
) -> bool {
    activation == binding.activation
        && bool::from(proof.as_bytes().ct_eq(binding.proof.as_bytes()))
        && credential == binding.credential
        && binding.host_pid.is_none_or(|host| host == pid)
}

fn host_binding<'a>(
    s: &'a mut crate::state::ServerState,
    client: ClientId,
    agent: &ResourceId,
    activation: &str,
    proof: &str,
    generation: Option<u64>,
) -> Result<&'a mut NativeBinding, CommandResult> {
    let binding = authenticated_host_binding(s, client, agent, activation, proof, generation)?;
    if binding.closed {
        return Err(closed("native agent session is closed"));
    }
    Ok(binding)
}

pub(crate) async fn handle(ctx: &CommandContext, request: NativeRequest) -> CommandResult {
    let state = &ctx.state;
    let client = ctx.client_id;
    if state.with(|s| s.native_agents().sealed)
        && let Some(result) = sealed_request(state, &request)
    {
        return result;
    }
    match request {
        NativeRequest::ListIntegrations {} => integrations::list().await,
        NativeRequest::LaunchIntegration {
            integration,
            session,
            cwd,
        } => integrations::launch(ctx, integration, session, cwd).await,
        NativeRequest::Spawn { spec } => spawn(ctx, &spec),
        NativeRequest::Command {
            agent_session_id,
            command,
        } => json(&state.with_mut(|s| submit_command(s, client, &agent_session_id, command))),
        NativeRequest::Register {
            agent_session_id,
            activation,
            binding_proof,
        } => state.with_mut(|s| {
            register_controller(s, client, &agent_session_id, &activation, &binding_proof)
        }),
        NativeRequest::Receive {
            agent_session_id,
            activation,
            binding_proof,
            generation,
        } => {
            receive(
                ctx,
                &agent_session_id,
                &activation,
                &binding_proof,
                generation,
            )
            .await
        }
        NativeRequest::Complete {
            agent_session_id,
            activation,
            binding_proof,
            generation,
            result,
        } => state.with_mut(|s| {
            complete(
                s,
                client,
                agent_session_id,
                &activation,
                &binding_proof,
                generation,
                result,
            )
        }),
        NativeRequest::Publish {
            agent_session_id,
            activation,
            binding_proof,
            generation,
            publication_id,
            update,
        } => {
            publish(
                ctx,
                agent_session_id,
                activation,
                binding_proof,
                generation,
                publication_id,
                update,
            )
            .await
        }
        NativeRequest::Close {
            agent_session_id,
            activation,
            binding_proof,
            generation,
        } => {
            close(
                ctx,
                &agent_session_id,
                &activation,
                &binding_proof,
                generation,
            )
            .await
        }
    }
}

fn sealed_request(state: &SharedState, request: &NativeRequest) -> Option<CommandResult> {
    match request {
        NativeRequest::Command { agent_session_id, command } => {
            let prior = state.with(|s| {
                s.native_agents().bindings.get(agent_session_id)
                    .and_then(|binding| binding.operations.get(&command.operation_id))
                    .filter(|operation| operation.command == *command)
                    .map(|operation| operation.result.clone())
            });
            Some(json(&prior.unwrap_or_else(|| refused_command(
                command.operation_id,
                "daemon upgrade cut is sealed; no model input was dispatched",
            ))))
        }
        NativeRequest::Spawn { .. } | NativeRequest::LaunchIntegration { .. } => Some(CommandResult::Error {
            code: ErrorCode::Overflow,
            message: "daemon upgrade is in progress; no agent was started; reconnect before launching again".into(),
        }),
        NativeRequest::Complete { .. }
        | NativeRequest::Publish { .. } | NativeRequest::Close { .. } => {
            Some(CommandResult::Error {
                code: ErrorCode::Overflow,
                message: "native broker upgrade cut is sealed; retry the identical publication/receipt after reconnect".into(),
            })
        }
        NativeRequest::Register { .. } | NativeRequest::Receive { .. }
        | NativeRequest::ListIntegrations {} => None,
    }
}

fn refused_command(operation_id: u64, detail: &str) -> NativeCommandResult {
    NativeCommandResult {
        operation_id,
        disposition: NativeDisposition::Refused,
        detail: Some(detail.into()),
    }
}

fn submit_command(
    s: &mut crate::state::ServerState,
    client: ClientId,
    agent: &ResourceId,
    command: NativeCommand,
) -> NativeCommandResult {
    let operation_id = command.operation_id;
    let Some(binding) = s.native_agents().bindings.get(agent) else {
        return refused_command(operation_id, "resource has no native controller");
    };
    if s.authenticated_credential(client)
        .is_some_and(|credential| credential.id == binding.credential)
    {
        return refused_command(
            operation_id,
            "SDK model tools cannot send native input to their own provider session",
        );
    }
    if let Some(prior) = binding.operations.get(&operation_id) {
        return if prior.command == command {
            prior.result.clone()
        } else {
            refused_command(
                operation_id,
                "operation identity conflicts with its reserved payload",
            )
        };
    }
    let parent = binding.parent.clone();
    let Some(core_parent) = s.resource_from_wire(&parent) else {
        return refused_command(operation_id, "native guardian is closed");
    };
    if !native_source_live(s, agent, core_parent) {
        return s.native_agents_mut().bindings.get_mut(agent).map_or_else(
            || refused_command(operation_id, "native controller disappeared"),
            |binding| binding.refuse(command, client.0, "native source or guardian closed"),
        );
    }
    let answer_requires_human = matches!(command.intent, NativeIntent::Answer { .. });
    if native_input_blocked(
        s,
        client,
        core_parent,
        &parent,
        agent,
        answer_requires_human,
    ) {
        return s.native_agents_mut().bindings.get_mut(agent).map_or_else(
            || refused_command(operation_id, "native controller disappeared"),
            |binding| binding.refuse(command, client.0, "input authority, human permission role, or shared guardian lease refused the native command"),
        );
    }
    let result = s
        .native_agents_mut()
        .bindings
        .get_mut(agent)
        .map(|binding| binding.submit(command, client.0));
    if let Some(wake) = s.native_agents().wakes.get(agent) {
        wake.notify_waiters();
    }
    result.unwrap_or_else(|| refused_command(operation_id, "native controller disappeared"))
}

fn native_source_live(
    s: &crate::state::ServerState,
    agent: &ResourceId,
    core_parent: phux_core::ids::ResourceId,
) -> bool {
    let source_live = s
        .resource_from_wire(agent)
        .and_then(|core| s.resource_handle(core))
        .and_then(|handle| handle.agent_session().ok())
        .is_some_and(|session| !session.ended.load(std::sync::atomic::Ordering::Relaxed));
    let guardian_live = s
        .registry()
        .record(core_parent)
        .and_then(|record| record.activation.as_ref())
        .is_some_and(|activation| activation.state.is_current());
    source_live && guardian_live
}

fn native_input_blocked(
    s: &crate::state::ServerState,
    client: ClientId,
    core_parent: phux_core::ids::ResourceId,
    parent: &ResourceId,
    agent: &ResourceId,
    answer_requires_human: bool,
) -> bool {
    s.connection_revoked(client)
        || s.input_blocked(core_parent, client)
        || s.is_viewer(client, parent)
        || s.is_viewer(client, agent)
        || (answer_requires_human
            && !s
                .connection_grant(client)
                .is_some_and(|grant| grant.kind() == phux_core::actor::PrincipalKind::Human))
}

fn register_controller(
    s: &mut crate::state::ServerState,
    client: ClientId,
    agent: &ResourceId,
    activation: &str,
    proof: &str,
) -> CommandResult {
    let generation = {
        let binding = match authenticated_host_binding(s, client, agent, activation, proof, None) {
            Ok(binding) => binding,
            Err(error) => return error,
        };
        if binding.closed {
            return json(&serde_json::json!({"generation":binding.generation}));
        }
        let Some(generation) = binding.generation.checked_add(1) else {
            return refusal("native generation exhausted");
        };
        binding.generation = generation;
        generation
    };
    if let Some(wake) = s.native_agents().wakes.get(agent) {
        wake.notify_waiters();
    }
    json(&serde_json::json!({"generation":generation}))
}

pub(super) async fn receive(
    ctx: &CommandContext,
    agent: &ResourceId,
    activation: &str,
    proof: &str,
    generation: u64,
) -> CommandResult {
    let state = &ctx.state;
    loop {
        let wake = state.with(|s| s.native_agents().wakes.get(agent).cloned());
        let Some(wake) = wake else {
            return closed("native agent session is gone");
        };
        let notified = wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let received = state.with_mut(|s| {
            let sealed = s.native_agents().sealed;
            let binding =
                host_binding(s, ctx.client_id, agent, activation, proof, Some(generation))?;
            // Do not dispatch against stale questions/turns while an
            // accepted SDK update is crossing the actor/registry seam.
            if sealed
                || binding
                    .publications
                    .back()
                    .is_some_and(|publication| publication.receipt.is_none())
            {
                return Ok(None);
            }
            refuse_undeliverable(s, agent);
            host_binding(s, ctx.client_id, agent, activation, proof, Some(generation))
                .map(NativeBinding::receive)
        });
        match received {
            Ok(Some(command)) => return json(&command),
            Err(error) => return error,
            Ok(None) => {}
        }
        tokio::select! {
            () = &mut notified => {},
            () = ctx.token.cancelled() => return refusal("native receive connection closed")
        }
    }
}

pub(super) fn complete(
    s: &mut crate::state::ServerState,
    client: ClientId,
    agent: ResourceId,
    activation: &str,
    proof: &str,
    generation: u64,
    result: NativeCommandResult,
) -> CommandResult {
    let (parent, changed) = {
        let binding = match host_binding(s, client, &agent, activation, proof, Some(generation)) {
            Ok(binding) => binding,
            Err(error) => return error,
        };
        let changed = binding
            .operations
            .get(&result.operation_id)
            .is_none_or(|operation| operation.result != result);
        if let Err(error) = binding.complete(result.clone()) {
            return refusal(error);
        }
        (binding.parent.clone(), changed)
    };
    if changed {
        let event = AgentEvent::NativeAgentResult {
            agent_session_id: agent.clone(),
            operation_id: result.operation_id,
            disposition: result.disposition,
            detail: result.detail,
        };
        s.record_and_fanout(
            crate::state::EventRecord::new(Some(agent), event)
                .with_parent(Some(parent))
                .with_actor(Some(client)),
        );
    }
    CommandResult::Ok
}
