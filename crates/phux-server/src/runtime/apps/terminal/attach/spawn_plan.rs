//! Spawn planning, refusal, and the spawned pane output pump.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;

/// The resource record a satellite Terminal spawn forwards (bind request,
/// idempotency key, `retain_secs`), when the consumer sent any; the
/// satellite applies them itself.
pub(super) fn forwarded_resource(
    bind_instance: bool,
    idempotency_key: Option<phux_protocol::ids::IdempotencyKey>,
    retain_secs: Option<u32>,
) -> Option<Box<phux_protocol::wire::frame::SpawnResource>> {
    (bind_instance || idempotency_key.is_some() || retain_secs.is_some()).then(|| {
        Box::new(
            phux_protocol::wire::frame::SpawnResource::default()
                .with_bind_instance(bind_instance)
                .with_idempotency_key(idempotency_key)
                .with_retain_secs(retain_secs),
        )
    })
}

/// The success reply for a spawn, bound to `instance` when requested
/// (ADR-0109).
pub(crate) const fn spawned_result(
    id: phux_protocol::ids::ResourceId,
    instance: Option<phux_protocol::ids::ServerInstance>,
) -> SpawnResult {
    match instance {
        Some(instance) => SpawnResult::OkBound { id, instance },
        None => SpawnResult::Ok(id),
    }
}

pub(super) fn log_spawn_request(
    client_id: ClientId,
    request_id: u32,
    request: &SpawnRequest,
    initial_size: Option<(u16, u16)>,
) {
    debug!(
        ?client_id,
        request_id,
        group = ?request.group,
        command = ?request.command,
        cwd = ?request.cwd,
        env_count = request.env.as_ref().map_or(0, Vec::len),
        satellite = ?request.satellite,
        owner_terminal = ?request.owner_terminal,
        initial_size = ?initial_size,
        "SPAWN_RESOURCE",
    );
}

/// What one `SPAWN_RESOURCE` asks the PTY layer to build.
pub(super) struct PaneSpawnPlan {
    /// The child to exec, fully configured from the wire frame.
    pub(super) builder: phux_terminal::pty::CommandBuilder,
    /// Scrollback rows the new pane retains.
    pub(super) scrollback: phux_config::ScrollbackLimits,
    /// Host palette the pane starts with, when the spawner advertised one.
    pub(super) default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    /// Opaque native agent-session provenance to install before publication.
    pub(super) agent_session: Option<Vec<u8>>,
    /// `(cols, rows)` to build the pane's grid and PTY at.
    pub(super) initial_size: Option<(u16, u16)>,
    /// Who asked, for the pane's `pane_spawned` stamp (ADR-0123).
    pub(super) attribution: crate::runtime::commands::SpawnAttribution,
}

/// Spawn the PTY-backed pane into the resolved owner's window, mapping both
/// failure shapes onto the typed wire refusal they are logged with.
pub(super) fn spawn_pane_or_refusal(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    ownership: &SpawnOwnership,
    root_token: &CancellationToken,
    plan: PaneSpawnPlan,
) -> Result<ResourceId, SpawnError> {
    match spawn_pane_with_pty_and_colors(
        state,
        ownership,
        plan.builder,
        plan.scrollback,
        root_token,
        plan.default_colors,
        plan.agent_session,
        plan.initial_size,
        plan.attribution,
    ) {
        Ok(Some(id)) => Ok(id),
        Ok(None) => {
            warn!(
                ?client_id,
                request_id, "SPAWN_RESOURCE: selected owner has no session to host the pane",
            );
            Err(SpawnError::SpawnFailed(
                "selected owner has no session to host the pane".to_owned(),
            ))
        }
        Err(err) => {
            warn!(
                ?client_id,
                request_id,
                error = %err,
                "SPAWN_RESOURCE: failed to spawn pane actor",
            );
            Err(SpawnError::SpawnFailed(format!("{err}")))
        }
    }
}

/// The pane spawned but its handle vanished: reap it and refuse the spawn so
/// the client is not left waiting.
pub(super) async fn refuse_vanished_pane_handle(
    state: &SharedState,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    client_id: ClientId,
    request_id: u32,
    core_terminal_id: ResourceId,
) {
    warn!(
        ?client_id,
        request_id,
        ?core_terminal_id,
        "SPAWN_RESOURCE: spawn succeeded but TerminalHandle vanished",
    );
    state.with_mut(|s| {
        let _ = crate::runtime::client::reap_pane_journaling_close(s, core_terminal_id);
    });
    refuse_spawn(
        out_tx,
        request_id,
        SpawnError::SpawnFailed(
            "internal state inconsistency: handle missing after spawn".to_owned(),
        ),
    )
    .await;
}

/// Reply to a `SPAWN_RESOURCE` with a typed refusal.
pub(crate) async fn refuse_spawn(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    request_id: u32,
    error: SpawnError,
) {
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::Err(error),
        }))
        .await;
}

/// A zero axis means "geometry unknown" (SPEC §10.5): use the default.
pub(super) fn usable_initial_size(initial_size: Option<(u16, u16)>) -> Option<(u16, u16)> {
    initial_size.filter(|&(cols, rows)| cols > 0 && rows > 0)
}

/// Stateless validation of a local spawn: the provenance bound, and the
/// single default Group ([`crate::state::DEFAULT_GROUP_ID`]).
pub(super) fn validate_local_spawn(
    agent_session: Option<&[u8]>,
    group: GroupId,
) -> Result<(), SpawnError> {
    if agent_session
        .is_some_and(|value| value.is_empty() || value.len() > MAX_AGENT_SESSION_RECORD_BYTES)
    {
        return Err(SpawnError::SpawnFailed(
            "agent-session provenance must contain 1..=4096 bytes".to_owned(),
        ));
    }
    if group != crate::state::DEFAULT_GROUP_ID {
        return Err(SpawnError::GroupNotFound);
    }
    Ok(())
}

/// Build the child's argv; no command runs the default shell.
pub(super) fn spawn_argv_builder(
    state: &SharedState,
    command: Option<Vec<String>>,
) -> phux_terminal::pty::CommandBuilder {
    crate::runtime::commands::argv_command(command).unwrap_or_else(|| {
        let (shell, login_shell) = state.with(|s| (s.shell().to_owned(), s.login_shell()));
        crate::terminal_actor::default_shell_command(&shell, login_shell)
    })
}

/// Build the `CommandBuilder` a `SPAWN_RESOURCE` execs. `TERM` layers, last
/// wins: server `defaults.term`, the spawn's `term`, then an `env` entry. An
/// explicit `cwd` wins over `defaults.cwd-inheritance`; `env` is additive.
pub(super) async fn build_spawn_command(
    state: &SharedState,
    client_id: ClientId,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    env: Option<Vec<(String, String)>>,
    term: Option<&str>,
) -> phux_terminal::pty::CommandBuilder {
    let mut builder = spawn_argv_builder(state, command);
    let default_term = state.with(|s| s.term().to_owned());
    crate::terminal_actor::apply_term(&mut builder, &default_term);
    if let Some(t) = term {
        crate::terminal_actor::apply_term(&mut builder, t);
    }
    if let Some(path) = cwd {
        builder.cwd(path);
    } else if let Some(path) = resolve_inherited_cwd(state, client_id).await {
        builder.cwd(path);
    }
    if let Some(pairs) = env {
        for (k, v) in pairs {
            builder.env(k, v);
        }
    }
    builder
}

/// Resolve which Terminal's window or session hosts the new pane: the named
/// owner, else the spawner's attached session, else the most recently active
/// one. A server with no session refuses rather than orphan the PTY.
pub(super) fn resolve_spawn_ownership(
    state: &SharedState,
    client_id: ClientId,
    owner_terminal: Option<phux_protocol::ids::ResourceId>,
) -> Result<SpawnOwnership, SpawnError> {
    if let Some(owner) = owner_terminal {
        if !matches!(owner, phux_protocol::ids::ResourceId::Local { .. })
            || state.with(|s| s.terminal_from_wire(&owner).is_none())
        {
            return Err(SpawnError::SpawnFailed(
                "owner terminal was not found on this server".to_owned(),
            ));
        }
        return Ok(SpawnOwnership::Terminal(owner));
    }
    let session = state.with(|s| {
        s.attached()
            .get(&client_id)
            .map(|c| c.session)
            .or_else(|| s.most_recently_touched_session())
            .or_else(|| s.fallback_session())
    });
    let Some(session) = session else {
        return Err(SpawnError::SpawnFailed(
            "server has no session to host the spawned pane".to_owned(),
        ));
    };
    Ok(SpawnOwnership::Session(session))
}

/// Auto-subscribe the spawning client to the new pane (or input to it would
/// be refused) and clone out its wire id, handle, and capabilities.
///
/// The subscription carries the spawner's mailbox, as `ATTACH_RESOURCE` and
/// an `AgentSession` spawn do: a spawner with no session `ATTACH` behind it (a
/// window browsing a session with per-terminal subscriptions, a headless
/// tool) is still pumped the pane's output, so it must pass the input gate
/// and receive terminal-scoped fanout the same way.
pub(super) fn subscribe_spawning_client(
    state: &SharedState,
    client_id: ClientId,
    core_terminal_id: ResourceId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) -> Option<(
    phux_protocol::ids::ResourceId,
    ResourceHandle,
    ClientCapabilities,
)> {
    state.with_mut(|s| {
        let wire_terminal_id = s.intern_terminal_wire(core_terminal_id);
        // ADR-0109: provenance before any subscription.
        s.record_spawn(core_terminal_id, client_id);
        let client_caps = s
            .attached()
            .get(&client_id)
            .map(|c| c.client_caps)
            .unwrap_or_default();
        s.subscribe_terminal(client_id, core_terminal_id, Some(out_tx.clone()));
        s.mark_if_viewer_session(client_id, &wire_terminal_id);
        s.resource_handle(core_terminal_id)
            .cloned()
            .map(|h| (wire_terminal_id, h, client_caps))
    })
}

/// Spawn the `SPAWN_RESOURCE` output pump and hand back its publication gate.
/// This pump owns its pane, so a lost generation reaps it.
pub(super) fn spawn_terminal_output_pump(
    ctx: OutputPumpContext,
    output_rx: tokio::sync::broadcast::Receiver<PaneOutput>,
    state: &SharedState,
    connection_token: &CancellationToken,
    core_terminal_id: ResourceId,
    output_pumps: &mut JoinSet<()>,
) -> oneshot::Sender<OutputPumpStart> {
    let pump_state = state.clone();
    let pump_connection_token = connection_token.clone();
    let (gate_tx, gate_rx) = oneshot::channel::<OutputPumpStart>();
    pump::spawn_tracked(
        state,
        ctx.client_id,
        core_terminal_id,
        Some(output_pumps),
        ctx.drain.clone(),
        async move {
            let Some(fault) = run_output_pump(&ctx, gate_rx, output_rx).await else {
                return;
            };
            match fault {
                PumpFault::OutboundClosed
                | PumpFault::TombstoneNotQueued
                | PumpFault::ReplayAbandoned
                | PumpFault::PaneGone => {}
                PumpFault::GenerationLost => {
                    let reaped = pump_state.with_mut(|s| {
                        crate::runtime::client::reap_pane_journaling_close(s, core_terminal_id)
                    });
                    // A pane that already closed ended with RESOURCE_CLOSED;
                    // losing the generation on the way out is no reason to
                    // drop the client.
                    if reaped {
                        warn!(
                            ?core_terminal_id,
                            "spawn pump lost its generation; reaped the pane, closing the client"
                        );
                        pump_connection_token.cancel();
                    }
                }
                PumpFault::PublicationNotActivated => {
                    warn!(
                        ?core_terminal_id,
                        "spawn pump publication not activated; closing the client"
                    );
                    pump_connection_token.cancel();
                }
            }
        },
    );
    gate_tx
}
