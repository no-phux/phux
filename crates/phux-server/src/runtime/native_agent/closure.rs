fn fence_closure(binding: &mut NativeBinding) -> Vec<NativeCommandResult> {
    if binding.closed {
        return Vec::new();
    }
    // Fence input immediately; durable terminal output can be retried independently.
    binding.closed = true;
    binding.view.connected = false;
    binding.view.capabilities = NativeCapabilities::default();
    binding.view.turn_id = None;
    binding.view.questions.clear();
    binding.pending.clear();
    binding.answered.clear();
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
    results
}

fn begin_closure(s: &mut crate::state::ServerState, agent: &ResourceId) {
    let Some(binding) = s.native_agents_mut().bindings.get_mut(agent) else {
        return;
    };
    let results = fence_closure(binding);
    let parent = binding.parent.clone();
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
    if let Some(wake) = s.native_agents().wakes.get(agent) {
        wake.notify_waiters();
    }
}

// Reserved above the host's publication sequence. The actor deduplicates this
// terminal transaction before checking ended/sealed state, including lost acks.
const CLOSURE_PUBLICATION_ID: u64 = u64::MAX;
async fn commit_closure(state: &SharedState, agent: &ResourceId) -> CommandResult {
    let view = state.with(|s| {
        s.native_agents().bindings.get(agent).and_then(|binding| {
            (binding.closed && !binding.closure_committed).then(|| binding.view.clone())
        })
    });
    let Some(view) = view else {
        return CommandResult::Ok;
    };
    let mut bytes = match serde_json::to_vec(
        &serde_json::json!({"type":"provider_raw","data":{"schema":NATIVE_SCHEMA,"update":{"type":"view","view":view}}}),
    ) {
        Ok(bytes) => bytes,
        Err(error) => return refusal(error.to_string()),
    };
    bytes.extend_from_slice(
        b"\n{\"type\":\"session_end\",\"data\":{\"reason\":\"native_closed\"}}\n",
    );
    let identity = phux_agent_session::session::NativeAppendIdentity {
        id: CLOSURE_PUBLICATION_ID,
        fingerprint: match fingerprint(&view) {
            Ok(value) => value,
            Err(error) => return refusal(error),
        },
    };
    let accepted = super::resource_commands::append_native_publication(
        state,
        agent,
        Bytes::from(bytes),
        identity,
    )
    .await;
    if let Err(error) = accepted {
        return error;
    }
    state.with_mut(|s| {
        if let Some(binding) = s.native_agents_mut().bindings.get_mut(agent) {
            binding.closure_committed = true;
        }
    });
    sync_ask_inventory(state, agent);
    CommandResult::Ok
}

fn schedule_closure(state: &SharedState, agent: &ResourceId) {
    let start = state.with_mut(|s| {
        let pending = s
            .native_agents()
            .bindings
            .get(agent)
            .is_some_and(|binding| binding.closed && !binding.closure_committed);
        pending && s.native_agents_mut().close_workers.insert(agent.clone())
    });
    if !start {
        return;
    }
    let state = state.clone();
    let agent = agent.clone();
    tokio::task::spawn_local(async move {
        while let CommandResult::Error {
            code: ErrorCode::Overflow,
            ..
        } = commit_closure(&state, &agent).await
        {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        state.with_mut(|s| {
            s.native_agents_mut().close_workers.remove(&agent);
        });
    });
}

pub(super) fn retry_pending_closures(state: &SharedState) {
    let agents: Vec<_> = state.with(|s| {
        s.native_agents()
            .bindings
            .values()
            .filter(|binding| binding.closed && !binding.closure_committed)
            .map(|binding| binding.agent.clone())
            .collect()
    });
    for agent in agents {
        schedule_closure(state, &agent);
    }
}

async fn close(
    ctx: &CommandContext,
    agent: &ResourceId,
    activation: &str,
    proof: &str,
    generation: u64,
) -> CommandResult {
    let authorized = ctx.state.with_mut(|s| {
        authenticated_host_binding(s, ctx.client_id, agent, activation, proof, Some(generation))
            .map(|_| ())
    });
    if let Err(error) = authorized {
        return error;
    }
    ctx.state.with_mut(|s| begin_closure(s, agent));
    schedule_closure(&ctx.state, agent);
    sync_ask_inventory(&ctx.state, agent);
    commit_closure(&ctx.state, agent).await
}
