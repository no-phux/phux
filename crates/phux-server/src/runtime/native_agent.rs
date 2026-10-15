//! Native launch transaction and exclusive authenticated controller broker.
use super::commands::CommandContext;
use crate::resource::agent_session::AgentSessionActor;
use crate::state::native_agent::{NativeBinding, NativePublication, fingerprint};
use crate::state::{ClientId, SharedState};
use bytes::Bytes;
use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::{
    MAX_NATIVE_OPERATIONS, NATIVE_HOST_SPEC_ENV, NATIVE_SCHEMA, NativeCapabilities, NativeCommand,
    NativeCommandResult, NativeDisposition, NativeIntent, NativePublicationReceipt, NativeRequest,
    NativeRequestPayload, NativeUpdate, NativeView, SpawnNativeAgent, SpawnedNativeAgent,
    validate_payload,
};
#[cfg(test)]
use phux_protocol::native_agent::{NativeOption, NativeQuestion, NativeQuestionItem, NativeState};
use phux_protocol::wire::frame::{AgentEvent, CommandResult, CommandValue, ErrorCode};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::Notify;
#[cfg(test)]
use tokio::sync::oneshot;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

fn refusal(message: impl Into<String>) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InvalidCommand,
        message: message.into(),
    }
}
fn denied(message: impl Into<String>) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::PermissionDenied,
        message: message.into(),
    }
}
fn closed(message: impl Into<String>) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::TerminalNotFound,
        message: message.into(),
    }
}
fn json<T: Serialize>(value: &T) -> CommandResult {
    match serde_json::to_string(value) {
        Ok(value) => CommandResult::OkWith(CommandValue::Json(value)),
        Err(error) => refusal(error.to_string()),
    }
}
fn host_binding<'a>(
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
    let core = s
        .terminal_from_wire(agent)
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
    let binding = s
        .native_agents_mut()
        .bindings
        .get_mut(agent)
        .ok_or_else(|| closed("no reserved native binding"))?;
    if activation != binding.activation
        || !bool::from(proof.as_bytes().ct_eq(binding.proof.as_bytes()))
        || credential != binding.credential
        || binding.host_pid.is_some_and(|host| host != pid)
    {
        return Err(denied(
            "native controller identity does not match the reservation",
        ));
    }
    if binding.closed {
        return Err(closed("native agent session is closed"));
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
pub(super) async fn handle(ctx: &CommandContext, request: NativeRequest) -> CommandResult {
    let state = &ctx.state;
    let client = ctx.client_id;
    if state.with(|s| s.native_agents().sealed)
        && let Some(result) = sealed_request(state, &request)
    {
        return result;
    }
    match request {
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
        NativeRequest::Spawn { .. } | NativeRequest::Complete { .. }
        | NativeRequest::Publish { .. } | NativeRequest::Close { .. } => {
            Some(CommandResult::Error {
                code: ErrorCode::Overflow,
                message: "native broker upgrade cut is sealed; retry the identical publication/receipt after reconnect".into(),
            })
        }
        NativeRequest::Register { .. } | NativeRequest::Receive { .. } => None,
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
    let Some(core_parent) = s.terminal_from_wire(&parent) else {
        return refused_command(operation_id, "native guardian is closed");
    };
    let source_live = s
        .terminal_from_wire(agent)
        .and_then(|core| s.resource_handle(core))
        .and_then(|handle| handle.agent_session().ok())
        .is_some_and(|session| !session.ended.load(std::sync::atomic::Ordering::Relaxed));
    let guardian_live = s
        .registry()
        .record(core_parent)
        .and_then(|record| record.activation.as_ref())
        .is_some_and(|activation| activation.state.is_current());
    if !source_live || !guardian_live {
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
        let binding = match host_binding(s, client, agent, activation, proof, None) {
            Ok(binding) => binding,
            Err(error) => return error,
        };
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

async fn receive(
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

fn complete(
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

fn close_controller(
    s: &mut crate::state::ServerState,
    client: ClientId,
    agent: &ResourceId,
    activation: &str,
    proof: &str,
    generation: u64,
) -> Result<NativeView, CommandResult> {
    let (view, parent, results) = {
        let binding = host_binding(s, client, agent, activation, proof, Some(generation))?;
        binding.closed = true;
        binding.view.connected = false;
        binding.view.questions.clear();
        binding.pending.clear();
        let mut results = Vec::new();
        for operation in binding.operations.values_mut().filter(|operation| {
            !matches!(
                operation.result.disposition,
                NativeDisposition::Completed | NativeDisposition::Refused
            )
        }) {
            operation.result.disposition = if operation.delivered {
                NativeDisposition::Unknown
            } else {
                NativeDisposition::Refused
            };
            operation.result.detail = Some("native controller closed".into());
            results.push(operation.result.clone());
        }
        (binding.view.clone(), binding.parent.clone(), results)
    };
    for result in results {
        s.record_and_fanout(
            crate::state::EventRecord::new(
                Some(agent.clone()),
                AgentEvent::NativeAgentResult {
                    agent_session_id: agent.clone(),
                    operation_id: result.operation_id,
                    disposition: result.disposition,
                    detail: result.detail,
                },
            )
            .with_parent(Some(parent.clone())),
        );
    }
    Ok(view)
}

async fn close(
    ctx: &CommandContext,
    agent: &ResourceId,
    activation: &str,
    proof: &str,
    generation: u64,
) -> CommandResult {
    let state = &ctx.state;
    let view = match state
        .with_mut(|s| close_controller(s, ctx.client_id, agent, activation, proof, generation))
    {
        Ok(view) => view,
        Err(error) => return error,
    };
    let bytes = serde_json::to_vec(
        &serde_json::json!({"type":"provider_raw","data":{"schema":NATIVE_SCHEMA,"update":{"type":"view","view":view}}}),
    );
    let result = match bytes {
        Ok(mut bytes) => {
            bytes.extend_from_slice(b"\n{\"type\":\"session_end\",\"data\":{\"reason\":\"native_controller_closed\"}}\n");
            super::resource_commands::append_native_output(state, agent, Bytes::from(bytes)).await
        }
        Err(error) => return refusal(error.to_string()),
    };
    if let Some(wake) = state.with(|s| s.native_agents().wakes.get(agent).cloned()) {
        wake.notify_waiters();
    }
    sync_ask_inventory(state, agent);
    result.map_or_else(|error| error, |_| CommandResult::Ok)
}
fn spawn(ctx: &CommandContext, spec: &SpawnNativeAgent) -> CommandResult {
    spawn_transaction(ctx, spec, None).map_or_else(|error| error, |spawned| json(&spawned))
}

/// An explicit saved native recipe uses the same transaction as new native
/// launches, while preserving the ordinary relaunch receipt/operation.
pub(super) fn spawn_relaunch(
    ctx: &CommandContext,
    spec: SpawnNativeAgent,
    operation: phux_protocol::ids::OperationId,
) -> CommandResult {
    let request = match NativeRequestPayload::new(NativeRequest::Spawn { spec }) {
        Ok(request) => request,
        Err(message) => return refusal(message),
    };
    let command = phux_protocol::wire::frame::Command::NativeAgent { request };
    if !matches!(
        ctx.state
            .with(|s| crate::policy::authorize_command(s, ctx.client_id, &command)),
        Ok(crate::policy::Admission::Run)
    ) {
        return denied("native relaunch requires Create+Bind on the exact organization session");
    }
    let phux_protocol::wire::frame::Command::NativeAgent { request } = command else {
        return refusal("invalid native relaunch carrier");
    };
    let NativeRequest::Spawn { spec } = request.into_request() else {
        return refusal("invalid native relaunch request");
    };
    match spawn_transaction(ctx, &spec, Some(operation)) {
        Ok(_) => CommandResult::Accepted { operation },
        Err(error) => error,
    }
}

struct NativeSpawnTarget {
    session: phux_core::ids::SessionId,
    existing: Option<phux_core::ids::ResourceId>,
}

struct PreparedNativeHost {
    command: portable_pty::CommandBuilder,
    launch: phux_core::TerminalLaunch,
    recipe: SpawnNativeAgent,
    public_spec: String,
}

struct NativeSpawnResources {
    parent: phux_core::ids::ResourceId,
    agent: phux_core::ids::ResourceId,
    parent_wire: ResourceId,
    agent_wire: ResourceId,
}

fn spawn_transaction(
    ctx: &CommandContext,
    spec: &SpawnNativeAgent,
    operation: Option<phux_protocol::ids::OperationId>,
) -> Result<SpawnedNativeAgent, CommandResult> {
    validate_spawn_spec(spec)?;
    let state = &ctx.state;
    let target = state.with(|s| resolve_spawn_target(s, spec))?;
    let PreparedNativeHost {
        mut command,
        launch,
        recipe,
        public_spec,
    } = prepare_native_host(state, spec)?;
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(|error| refusal(error.to_string()))?;
    let proof = hex::encode(random);
    let activation = hex::encode(&random[..16]);
    let delegation = delegate_native_host(ctx, target.session)?;
    let resources = match create_native_resources(
        ctx,
        spec,
        &target,
        launch,
        &mut command,
        &public_spec,
        operation,
    ) {
        Ok(resources) => resources,
        Err(error) => {
            super::delegation::revoke_delegated_agent(state, &delegation.credential.id);
            return Err(error);
        }
    };
    command.env("PHUX_SOCKET", &delegation.socket_path);
    command.env("PHUX_NATIVE_AGENT_ID", resources.agent_wire.to_string());
    command.env("PHUX_NATIVE_ACTIVATION", &activation);
    command.env("PHUX_NATIVE_BINDING", &proof);
    let binding = reserved_binding(
        spec,
        recipe,
        &resources,
        &activation,
        proof,
        &delegation.credential.id,
    );
    install_native_session(ctx, spec, &resources, binding, operation);
    launch_native_guardian(
        ctx,
        &resources,
        command,
        operation,
        target.existing.is_some(),
        &delegation.credential.id,
    )?;
    Ok(SpawnedNativeAgent {
        parent: resources.parent_wire,
        agent: resources.agent_wire,
        activation,
    })
}

fn validate_spawn_spec(spec: &SpawnNativeAgent) -> Result<(), CommandResult> {
    validate_payload(spec).map_err(refusal)?;
    if !matches!(spec.backend.as_str(), "omp-rpc" | "acp" | "node")
        || spec.provider.is_empty()
        || spec.command.first().is_none_or(String::is_empty)
    {
        return Err(refusal(
            "native backend/provider/executable is unsupported or absent",
        ));
    }
    if spec.env.keys().any(|key| {
        key.starts_with("PHUX_NATIVE_")
            || matches!(key.as_str(), "PHUX_SOCKET" | "PHUX_TERMINAL_ID")
    }) {
        return Err(refusal(
            "native launch environment cannot override private host binding",
        ));
    }
    validate_guardian_selection(spec)
}

fn validate_guardian_selection(spec: &SpawnNativeAgent) -> Result<(), CommandResult> {
    if spec.guardian.is_some() && spec.guardian_key.is_some() {
        return Err(refusal(
            "guardian wire id and organization key are mutually exclusive",
        ));
    }
    if spec.guardian_expected.is_some() && spec.guardian.is_none() && spec.guardian_key.is_none() {
        return Err(refusal(
            "an observed guardian revision requires an exact guardian target",
        ));
    }
    Ok(())
}

fn resolve_spawn_target(
    s: &crate::state::ServerState,
    spec: &SpawnNativeAgent,
) -> Result<NativeSpawnTarget, CommandResult> {
    if s.native_agents().sealed {
        return Err(CommandResult::Error {
            code: ErrorCode::Overflow,
            message: "native broker upgrade cut is sealed; no guardian was created".into(),
        });
    }
    let session = s
        .find_session_by_name(&spec.session)
        .ok_or_else(|| refusal("native launch requires an existing organization session"))?;
    let guardian = if let Some(wire) = &spec.guardian {
        if !wire.is_local() {
            return Err(refusal("native insertion requires a local guardian"));
        }
        Some(
            s.terminal_from_wire(wire)
                .ok_or_else(|| refusal("guardian does not exist"))?,
        )
    } else if let Some(key) = &spec.guardian_key {
        Some(
            s.registry()
                .resource_id(&crate::org_wire::core_resource_key(key))
                .ok_or_else(|| refusal("guardian record does not exist"))?,
        )
    } else {
        None
    };
    if let Some(guardian) = guardian {
        validate_existing_guardian(s, spec, session, guardian)?;
    }
    Ok(NativeSpawnTarget {
        session,
        existing: guardian,
    })
}

fn validate_existing_guardian(
    s: &crate::state::ServerState,
    spec: &SpawnNativeAgent,
    session: phux_core::ids::SessionId,
    guardian: phux_core::ids::ResourceId,
) -> Result<(), CommandResult> {
    let record = s
        .registry()
        .record(guardian)
        .ok_or_else(|| refusal("guardian record does not exist"))?;
    if record.kind != phux_core::ResourceKind::Terminal
        || s.registry().session_of(guardian) != Some(session)
    {
        return Err(refusal(
            "guardian must be a Terminal in the exact organization session",
        ));
    }
    if s.resource_handle(guardian).is_some()
        || record.activation.as_ref().is_some_and(|activation| {
            activation.state.is_current() || activation.state == phux_core::ExecState::Lost
        })
    {
        return Err(refusal(
            "guardian has a current, lost, or retained engine; no process may be replaced",
        ));
    }
    if let Some(expected) = spec.guardian_expected
        && crate::org_wire::core_revision(expected) != record.revision
    {
        return Err(CommandResult::Refused(crate::org_wire::refusal(
            &phux_core::Refusal::Conflict {
                current: record.revision,
            },
        )));
    }
    Ok(())
}

fn prepare_native_host(
    state: &SharedState,
    spec: &SpawnNativeAgent,
) -> Result<PreparedNativeHost, CommandResult> {
    let executable = state
        .with(crate::state::ServerState::native_host_executable)
        .map_err(|error| refusal(error.to_string()))?;
    let mut command = portable_pty::CommandBuilder::new(&executable);
    command.args(["agent", "host"]);
    if let Some(cwd) = &spec.cwd {
        command.cwd(cwd);
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    // Host-only startup carries the real local runtime configuration. The
    // portable saved recipe carries no bootstrap destination or SDK credentials.
    let startup = serde_json::to_string(spec).map_err(|error| refusal(error.to_string()))?;
    command.env(NATIVE_HOST_SPEC_ENV, startup);
    let mut recipe = spec.clone();
    recipe.guardian = None;
    recipe.guardian_key = None;
    recipe.guardian_expected = None;
    recipe.sdk_runtime = None;
    recipe.node_bridge = None;
    recipe.env.clear();
    let public_spec = serde_json::to_string(&recipe).map_err(|error| refusal(error.to_string()))?;
    let mut launch = super::commands::launch_of(Some(&command));
    launch
        .env
        .push((NATIVE_HOST_SPEC_ENV.into(), public_spec.clone()));
    Ok(PreparedNativeHost {
        command,
        launch,
        recipe,
        public_spec,
    })
}

fn delegate_native_host(
    ctx: &CommandContext,
    session: phux_core::ids::SessionId,
) -> Result<super::delegation::DelegatedAgent, CommandResult> {
    // Bind is mandatory. Optional model-tool verbs only narrow what the
    // original launcher actually holds; a Create+Bind launcher is sufficient.
    let delegated_verbs = ctx.state.with(|s| {
        use phux_protocol::kinds::{Verb, Verbs};
        let mut verbs = Verbs::of(&[Verb::Bind]);
        if let Some(grant) = s.connection_grant(ctx.client_id) {
            let group = s
                .idspace
                .session_wire(session)
                .map(phux_protocol::ids::SessionId::get);
            for verb in [Verb::Inventory, Verb::Observe, Verb::Input] {
                if grant.is_owner() || group.is_some_and(|group| grant.admits_session(verb, group))
                {
                    verbs = verbs.union(Verbs::of(&[verb]));
                }
            }
        }
        verbs
    });
    super::delegation::delegate_agent(
        &ctx.state,
        ctx.client_id,
        session,
        crate::delegation::DelegationRequest {
            verbs: delegated_verbs,
            ..Default::default()
        },
        ctx.input_lane.clone(),
        &ctx.root_token,
    )
    .map_err(|error| denied(error.to_string()))
}

fn create_native_resources(
    ctx: &CommandContext,
    spec: &SpawnNativeAgent,
    target: &NativeSpawnTarget,
    launch: phux_core::TerminalLaunch,
    command: &mut portable_pty::CommandBuilder,
    public_spec: &str,
    operation: Option<phux_protocol::ids::OperationId>,
) -> Result<NativeSpawnResources, CommandResult> {
    ctx.state.with_mut(|s| {
        let actor = s.actor_for(ctx.client_id);
        let parent = if let Some(parent) = target.existing {
            s.relaunch_native_guardian(
                parent,
                &launch,
                ctx.client_id,
                operation.as_ref().map(crate::org_wire::core_operation),
                spec.guardian_expected.map(crate::org_wire::core_revision),
            )
            .map_err(|error| CommandResult::Refused(crate::org_wire::refusal(&error)))?;
            parent
        } else {
            s.create_terminal(
                crate::state::TerminalTarget::Session(target.session),
                launch,
                phux_core::ExitPolicy::Retain,
                &actor,
            )
            .map_err(|error| CommandResult::Refused(crate::org_wire::refusal(&error)))?
            .1
        };
        let facet = phux_core::AgentFacet {
            provider: spec.provider.clone(),
            native_id: spec.native_id.clone(),
            state: None,
        };
        let agent = match s.create_agent_session(parent, &facet, &actor) {
            Ok(agent) => agent,
            Err(error) => {
                if target.existing.is_some() {
                    s.activation_ended(parent, phux_core::ExitOutcome::UNKNOWN, true);
                } else {
                    s.reap_terminal(parent);
                }
                return Err(CommandResult::Refused(crate::org_wire::refusal(&error)));
            }
        };
        let parent_wire = super::commands::register_pty_command(s, parent, command);
        let agent_wire = s.intern_terminal_wire(agent);
        s.metadata_set(
            &phux_protocol::wire::frame::Scope::Resource(parent_wire.clone()),
            "phux.native-launch/v1",
            public_spec.as_bytes().to_vec(),
        );
        Ok(NativeSpawnResources {
            parent,
            agent,
            parent_wire,
            agent_wire,
        })
    })
}

fn reserved_binding(
    spec: &SpawnNativeAgent,
    recipe: SpawnNativeAgent,
    resources: &NativeSpawnResources,
    activation: &str,
    proof: String,
    credential: &str,
) -> NativeBinding {
    NativeBinding {
        launch: recipe,
        agent: resources.agent_wire.clone(),
        parent: resources.parent_wire.clone(),
        activation: activation.into(),
        proof,
        credential: credential.into(),
        host_pid: None,
        generation: 0,
        view: NativeView {
            activation: activation.into(),
            backend: spec.backend.clone(),
            capabilities: NativeCapabilities::default(),
            connected: false,
            turn_id: None,
            provider_session_id: spec.native_id.clone(),
            questions: Vec::new(),
        },
        closed: false,
        operations: BTreeMap::new(),
        pending: VecDeque::new(),
        answered: BTreeMap::new(),
        publications: VecDeque::new(),
        publication_high_water: 0,
    }
}

fn install_native_session(
    ctx: &CommandContext,
    spec: &SpawnNativeAgent,
    resources: &NativeSpawnResources,
    binding: NativeBinding,
    operation: Option<phux_protocol::ids::OperationId>,
) {
    let state = &ctx.state;
    let token = ctx.root_token.child_token();
    let bundle = AgentSessionActor::build(
        resources.parent,
        &spec.provider,
        spec.native_id.as_deref(),
        token.clone(),
        state.with(crate::state::ServerState::agent_log_bytes),
    );
    state.with_mut(|s| {
        s.native_agents_mut()
            .bindings
            .insert(resources.agent_wire.clone(), binding);
        s.native_agents_mut()
            .wakes
            .insert(resources.agent_wire.clone(), Arc::new(Notify::new()));
        s.spawn_resource_actor(
            resources.agent,
            bundle.handle.clone(),
            token,
            bundle.actor.run(),
        );
        s.record_spawn(resources.parent, ctx.client_id);
        s.record_spawn(resources.agent, ctx.client_id);
        s.mark_if_viewer_session(ctx.client_id, &resources.parent_wire);
        s.mark_if_viewer_session(ctx.client_id, &resources.agent_wire);
        let announcement = crate::state::EventRecord::new(
            Some(resources.agent_wire.clone()),
            AgentEvent::ResourceSpawned {
                kind: phux_protocol::ids::ResourceKind::AgentSession,
                parent: Some(resources.parent_wire.clone()),
            },
        )
        .with_parent(Some(resources.parent_wire.clone()))
        .with_actor(Some(ctx.client_id))
        .with_operation_id(operation);
        s.record_and_fanout(announcement);
    });
    super::client::spawn_terminal_exit_watcher(
        state.clone(),
        resources.agent,
        Some(bundle.exit_notify),
        ctx.root_token.clone(),
        None,
    );
}

fn launch_native_guardian(
    ctx: &CommandContext,
    resources: &NativeSpawnResources,
    command: portable_pty::CommandBuilder,
    operation: Option<phux_protocol::ids::OperationId>,
    reused: bool,
    credential: &str,
) -> Result<(), CommandResult> {
    let state = &ctx.state;
    let launched = super::commands::launch_pane_actor_with_failure_policy(
        state,
        resources.parent,
        Some(command),
        super::commands::DEFAULT_SPAWN_DIMS,
        state.with(crate::state::ServerState::scrollback_limits),
        &ctx.root_token,
        None,
        super::commands::SpawnAttribution {
            actor: Some(ctx.client_id),
            operation_id: operation,
            ..Default::default()
        },
        reused,
    );
    if let Err(error) = launched {
        super::delegation::revoke_delegated_agent(state, credential);
        state.with_mut(|s| {
            s.reap_terminal(resources.agent);
            if !reused {
                s.reap_terminal(resources.parent);
            }
            s.native_agents_mut().bindings.remove(&resources.agent_wire);
            if let Some(wake) = s.native_agents_mut().wakes.remove(&resources.agent_wire) {
                wake.notify_waiters();
            }
        });
        return Err(refusal(error.to_string()));
    }
    Ok(())
}
async fn publish(
    ctx: &CommandContext,
    agent: ResourceId,
    activation: String,
    proof: String,
    generation: u64,
    id: u64,
    update: NativeUpdate,
) -> CommandResult {
    if id == 0 {
        return refusal("publication identity must be nonzero");
    }
    let fingerprint = match fingerprint(&update) {
        Ok(fingerprint) => fingerprint,
        Err(error) => return refusal(error),
    };
    let projected = ctx.state.with_mut(|s| {
        let binding = host_binding(
            s,
            ctx.client_id,
            &agent,
            &activation,
            &proof,
            Some(generation),
        )?;
        reserve_publication(binding, id, &fingerprint, &update)
    });
    let view = match projected {
        Ok(view) => view,
        Err(result) => return result,
    };
    let bytes = match encode_publication(&update, &view) {
        Ok(bytes) => bytes,
        Err(error) => return error,
    };
    let accepted = super::resource_commands::append_native_publication(
        &ctx.state,
        &agent,
        bytes,
        crate::resource::agent_session::NativeAppendIdentity { id, fingerprint },
    )
    .await;
    let accepted = match accepted {
        Ok(accepted) => accepted,
        Err(error) => {
            rollback_publication(&ctx.state, &agent, id);
            return error;
        }
    };
    let receipt = NativePublicationReceipt {
        publication_id: id,
        first_seq: accepted.first_seq,
        last_seq: accepted.last_seq,
    };
    acknowledge_publication(&ctx.state, &agent, view, &receipt, &update)
}

fn acknowledge_publication(
    state: &SharedState,
    agent: &ResourceId,
    view: NativeView,
    receipt: &NativePublicationReceipt,
    update: &NativeUpdate,
) -> CommandResult {
    if let Err(error) = persist_publication(state, agent, receipt.publication_id, view, receipt) {
        return CommandResult::Error {
            code: ErrorCode::Overflow,
            message: format!("native publication accepted but durable recipe pending: {error}"),
        };
    }
    // A question closing while the turn remains blocked must still
    // update the ask inventory, rather than leaving a stale question.
    if matches!(
        update,
        NativeUpdate::QuestionClosed { .. } | NativeUpdate::View { .. }
    ) {
        sync_ask_inventory(state, agent);
    }
    json(receipt)
}

fn reserve_publication(
    binding: &mut NativeBinding,
    id: u64,
    fingerprint: &str,
    update: &NativeUpdate,
) -> Result<NativeView, CommandResult> {
    if let Some(prior) = binding
        .publications
        .iter()
        .find(|publication| publication.id == id)
    {
        if prior.fingerprint != fingerprint {
            return Err(refusal("publication identity conflicts with its payload"));
        }
        if let Some(receipt) = &prior.receipt {
            return Err(json(receipt));
        }
        // The actor, not the requesting connection, owns append dedupe.
        // A cancelled/lost acknowledgment can retry exactly this identity.
        return prior
            .view
            .clone()
            .ok_or_else(|| refusal("pending publication has no saved projection"));
    }
    if id <= binding.publication_high_water {
        return Err(refusal(
            "publication receipt expired; old identity cannot be replayed",
        ));
    }
    if binding
        .publications
        .back()
        .is_some_and(|publication| publication.receipt.is_none())
    {
        return Err(refusal("prior publication still pending"));
    }
    let view = binding.project(update).map_err(refusal)?;
    binding.publications.push_back(NativePublication {
        id,
        fingerprint: fingerprint.into(),
        receipt: None,
        view: Some(view.clone()),
    });
    binding.publication_high_water = id;
    while binding.publications.len() > MAX_NATIVE_OPERATIONS {
        binding.publications.pop_front();
    }
    Ok(view)
}

fn encode_publication(update: &NativeUpdate, view: &NativeView) -> Result<Bytes, CommandResult> {
    let mut records = Vec::new();
    records.push(
        serde_json::json!({"type":"provider_raw","data":{"schema":NATIVE_SCHEMA,"update":update}}),
    );
    match update {
        NativeUpdate::State { state, .. } => {
            records.push(serde_json::json!({"type":"state","data":{"state":state}}));
        }
        NativeUpdate::QuestionOpened { question } => {
            let text = if question.message.is_empty() {
                &question.title
            } else {
                &question.message
            };
            records.push(serde_json::json!({"type":"ask","data":{"id":question.id,"question":text,"suggestions":question.options.iter().map(|option| &option.label).collect::<Vec<_>>()}}));
        }
        NativeUpdate::Transcript { entry } => {
            records.push(serde_json::json!({"type":"provider_raw","data":entry}));
        }
        NativeUpdate::Notice { level, text } => {
            records.push(
                serde_json::json!({"type":"notification","data":{"level":level,"message":text}}),
            );
        }
        _ => {}
    }
    if !matches!(update, NativeUpdate::View { .. }) {
        records.push(serde_json::json!({"type":"provider_raw","data":{"schema":NATIVE_SCHEMA,"update":{"type":"view","view":view}}}));
    }
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, &record).map_err(|error| refusal(error.to_string()))?;
        bytes.push(b'\n');
    }
    Ok(Bytes::from(bytes))
}

fn persist_publication(
    state: &SharedState,
    agent: &ResourceId,
    id: u64,
    view: NativeView,
    receipt: &NativePublicationReceipt,
) -> Result<(), phux_core::Refusal> {
    state.with_mut(|s| {
        s.persist_native_recipe(agent, &view)?;
        if let Some(binding) = s.native_agents_mut().bindings.get_mut(agent) {
            if let Some(publication) = binding
                .publications
                .iter_mut()
                .find(|publication| publication.id == id)
            {
                publication.receipt = Some(receipt.clone());
                publication.view = None;
            }
            binding.view = view;
        }
        let native_id = s
            .native_agents()
            .bindings
            .get(agent)
            .and_then(|binding| binding.view.provider_session_id.clone());
        if let Some(core) = s.terminal_from_wire(agent)
            && let Some(facet) = s
                .registry_mut()
                .resource_mut(core)
                .and_then(|resource| resource.agent.as_mut())
        {
            facet.native_id = native_id;
        }
        if let Some(binding) = s.native_agents().bindings.get(agent) {
            let parent = binding.parent.clone();
            if let Ok(bytes) = serde_json::to_vec(&binding.launch) {
                s.metadata_set(
                    &phux_protocol::wire::frame::Scope::Resource(parent),
                    "phux.native-launch/v1",
                    bytes,
                );
            }
        }
        if let Some(wake) = s.native_agents().wakes.get(agent) {
            wake.notify_waiters();
        }
        Ok(())
    })
}

fn rollback_publication(state: &SharedState, agent: &ResourceId, id: u64) {
    state.with_mut(|s| {
        if let Some(binding) = s.native_agents_mut().bindings.get_mut(agent)
            && binding
                .publications
                .back()
                .is_some_and(|publication| publication.id == id && publication.receipt.is_none())
        {
            binding.publications.pop_back();
            binding.publication_high_water = binding
                .publications
                .back()
                .map_or(0, |publication| publication.id);
        }
        if let Some(wake) = s.native_agents().wakes.get(agent) {
            wake.notify_waiters();
        }
    });
}
fn sync_ask_inventory(state: &SharedState, agent: &ResourceId) {
    let published = state.with_mut(|s| {
        let binding = s.native_agents().bindings.get(agent)?;
        let parent = binding.parent.clone();
        let question = binding.view.questions.last().cloned();
        let core = s.terminal_from_wire(&parent)?;
        let payload = if let Some(question) = question {
            let text = if question.message.is_empty() {
                question.title
            } else {
                question.message
            };
            s.report_stream_ask(
                core,
                crate::agent_asked::AskedPayload {
                    id: question.id,
                    question: text,
                    suggestions: question
                        .options
                        .into_iter()
                        .map(|option| option.label)
                        .collect(),
                    elapsed_seconds: None,
                },
            )
            .emit_payload()
        } else {
            s.retract_agent_asked(core, crate::agent_asked::AskedSource::Stream);
            None
        };
        crate::hub::metadata_mirror::publish_asked_flag(s, &parent, s.agent_is_asked(core));
        payload.map(|payload| (parent, payload))
    });
    if let Some((parent, payload)) = published {
        super::client::broadcast_event(state, Some(&parent), &payload.into_event());
    }
}

/// Recheck the original caller at the actual dispatch point. A queued
/// reservation does not outlive a revoked grant, stolen lease, or question.
fn refuse_undeliverable(s: &mut crate::state::ServerState, agent: &ResourceId) {
    let Some(binding) = s.native_agents().bindings.get(agent) else {
        return;
    };
    let parent = binding.parent.clone();
    let Some(core_parent) = s.terminal_from_wire(&parent) else {
        return;
    };
    let candidates: Vec<_> = binding
        .pending
        .iter()
        .filter_map(|id| binding.operations.get(id))
        .map(|operation| (operation.command.clone(), ClientId(operation.requester)))
        .collect();
    for (command, requester) in candidates {
        let current = s.native_agents().bindings.get(agent);
        let valid = current.is_some_and(|binding| {
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
        });
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
pub(super) async fn source_exited(state: &SharedState, resource: phux_core::ids::ResourceId) {
    let updates = state.with_mut(|s| {
        let Some(wire) = s.terminal_wire_of(resource) else {
            return Vec::new();
        };
        let mut updates = Vec::new();
        let mut results = Vec::new();
        for binding in s
            .native_agents_mut()
            .bindings
            .values_mut()
            .filter(|binding| binding.parent == wire || binding.agent == wire)
        {
            if binding.closed {
                continue;
            }
            binding.closed = true;
            binding.view.connected = false;
            binding.view.questions.clear();
            binding.pending.clear();
            for operation in binding.operations.values_mut().filter(|operation| {
                !matches!(
                    operation.result.disposition,
                    NativeDisposition::Completed | NativeDisposition::Refused
                )
            }) {
                operation.result.disposition = if operation.delivered {
                    NativeDisposition::Unknown
                } else {
                    NativeDisposition::Refused
                };
                operation.result.detail = Some("native source closed".into());
                results.push((
                    binding.agent.clone(),
                    binding.parent.clone(),
                    operation.result.clone(),
                ));
            }
            updates.push((binding.agent.clone(), binding.view.clone()));
        }
        for (agent, parent, result) in results {
            s.record_and_fanout(
                crate::state::EventRecord::new(
                    Some(agent.clone()),
                    AgentEvent::NativeAgentResult {
                        agent_session_id: agent,
                        operation_id: result.operation_id,
                        disposition: result.disposition,
                        detail: result.detail,
                    },
                )
                .with_parent(Some(parent)),
            );
        }
        for (agent, _) in &updates {
            if let Some(wake) = s.native_agents().wakes.get(agent) {
                wake.notify_waiters();
            }
        }
        updates
    });
    for (agent, view) in updates {
        if let Ok(mut bytes) = serde_json::to_vec(
            &serde_json::json!({"type":"provider_raw","data":{"schema":NATIVE_SCHEMA,"update":{"type":"view","view":view}}}),
        ) {
            bytes.extend_from_slice(
                b"\n{\"type\":\"session_end\",\"data\":{\"reason\":\"native_source_closed\"}}\n",
            );
            let _ =
                super::resource_commands::append_native_output(state, &agent, Bytes::from(bytes))
                    .await;
        }
        sync_ask_inventory(state, &agent);
    }
}

#[cfg(test)]
mod tests;
