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
    if !matches!(spec.backend.as_str(), "acp")
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
    let guardian = selected_guardian(s, spec)?;
    if let Some(guardian) = guardian {
        validate_existing_guardian(s, spec, session, guardian)?;
    }
    Ok(NativeSpawnTarget {
        session,
        existing: guardian,
    })
}

fn selected_guardian(
    s: &crate::state::ServerState,
    spec: &SpawnNativeAgent,
) -> Result<Option<phux_core::ids::ResourceId>, CommandResult> {
    if let Some(wire) = &spec.guardian {
        if !wire.is_local() {
            return Err(refusal("native insertion requires a local guardian"));
        }
        return s
            .terminal_from_wire(wire)
            .map(Some)
            .ok_or_else(|| refusal("guardian does not exist"));
    }
    if let Some(key) = &spec.guardian_key {
        return s
            .registry()
            .resource_id(&crate::org_wire::core_resource_key(key))
            .map(Some)
            .ok_or_else(|| refusal("guardian record does not exist"));
    }
    Ok(None)
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
            let group = crate::policy::GroupPoint::of(s, session);
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
        closure_committed: false,
        publication_committed: 0,
        evidence_seq: 0,
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
            // Keep failed startup diagnostics on the durable guardian.
            retain_secs: Some(600),
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
