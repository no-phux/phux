//! `SPAWN_RESOURCE` handling, including satellite relay.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;
use phux_terminal::actor::TerminalResource as _;

/// The decoded `SPAWN_RESOURCE` payload, minus `request_id`.
#[derive(Debug)]
pub(crate) struct SpawnRequest {
    /// Group under which to spawn (v0.1 servers expose `GroupId(1)`).
    pub(crate) group: GroupId,
    /// Command + argv, or `None` for the server's default shell.
    pub(crate) command: Option<Vec<String>>,
    /// Working directory, or `None` for the server's default policy.
    pub(crate) cwd: Option<String>,
    /// Environment pairs, `None` = inherit the server's environment.
    pub(crate) env: Option<Vec<(String, String)>>,
    /// First-class `TERM` override.
    pub(crate) term: Option<String>,
    /// Satellite host to route the spawn to, `None` = local.
    pub(crate) satellite: Option<phux_protocol::ids::SatelliteHost>,
    /// Existing Terminal on the spawn's host whose exact window owns the new pane.
    pub(crate) owner_terminal: Option<phux_protocol::ids::ResourceId>,
    /// Opaque native agent-session provenance to install before publication.
    pub(crate) agent_session: Option<Vec<u8>>,
    /// `(cols, rows)` for the pane's grid and PTY, `None` for the default.
    pub(crate) initial_size: Option<(u16, u16)>,
    /// Kind, parent, and provenance fields; `None` is a plain Terminal.
    pub(crate) resource: Option<Box<phux_protocol::wire::frame::SpawnResource>>,
}

/// Only an owner explicitly addressed to this satellite can cross its link.
/// Provenance remains an independent local-only restriction.
fn satellite_spawn_owner(
    host: &phux_protocol::ids::SatelliteHost,
    owner: Option<phux_protocol::ids::ResourceId>,
    has_agent_session: bool,
) -> Result<Option<u32>, SpawnError> {
    if has_agent_session {
        return Err(SpawnError::SpawnFailed(
            "agent-session provenance is local-only".to_owned(),
        ));
    }
    match owner {
        None => Ok(None),
        Some(phux_protocol::ids::ResourceId::Satellite {
            host: owner_host,
            id,
        }) if owner_host == *host => Ok(Some(id)),
        Some(_) => Err(SpawnError::SpawnFailed(
            "owner terminal must belong to the requested satellite host".to_owned(),
        )),
    }
}

/// Relay one satellite-addressed spawn over the hub link (L1 §9.1); no route
/// is the typed configuration refusal.
async fn relay_spawn_to_satellite(
    state: &SharedState,
    host: &phux_protocol::ids::SatelliteHost,
    spawn: SatelliteSpawn,
) -> SpawnResult {
    let Some(relay) = state.with(|s| s.hub_relay(host)) else {
        debug!(
            satellite = %host,
            "SPAWN_RESOURCE: no route to satellite (non-hub server, or host not in the registry)",
        );
        return SpawnResult::Err(SpawnError::UnsupportedSatelliteRoute);
    };
    relay.spawn(spawn).await
}

/// ADR-0109: record which hub consumer spawned a satellite resource (the
/// satellite sees one link identity), before any consumer can learn the id.
fn record_satellite_spawn(state: &SharedState, client_id: ClientId, result: &SpawnResult) {
    if let Some(phux_protocol::ids::ResourceId::Satellite { host, id }) = result.spawned_id() {
        let instance = result.instance();
        state.with_mut(|s| s.hub_record_satellite_spawn(host.clone(), *id, instance, client_id));
    }
}

/// Relay a satellite-targeted spawn (or reply with its validation refusal)
/// and reply with the re-tagged result.
pub(crate) async fn dispatch_satellite_spawn(
    state: &SharedState,
    client_id: ClientId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    request_id: u32,
    host: &phux_protocol::ids::SatelliteHost,
    spawn: Result<SatelliteSpawn, SpawnError>,
) {
    let result = match spawn {
        Ok(spawn) => relay_spawn_to_satellite(state, host, spawn).await,
        Err(error) => SpawnResult::Err(error),
    };
    record_satellite_spawn(state, client_id, &result);
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::ResourceSpawned {
            request_id,
            result,
        }))
        .await;
}

/// Handle `SPAWN_RESOURCE` (SPEC §10.1): spawn a PTY-backed Terminal into
/// the spawning client's current session (or the most recently active one
/// for a detached spawner), auto-subscribe the spawner, and publish its first
/// generation. Refusals ride the typed `RESOURCE_SPAWNED` reply. Satellite
/// spawns are relayed over the hub link and never touch local dispatch;
/// other kinds are dispatched by kind (ADR-0102).
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "linear orchestration, one named helper per stage"
)]
pub(crate) async fn handle_spawn_terminal(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    request: SpawnRequest,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    root_token: &CancellationToken,
    connection_token: &CancellationToken,
    output_pumps: &mut JoinSet<()>,
    defer_subscription: bool,
) {
    let Some(profile) = bootstrap_stream_profile(bootstrap_profile) else {
        let _ = out_tx
            .send(Outbound::Frame(FrameKind::Error {
                request_id: Some(request_id),
                code: ErrorCode::CodecUnavailable,
                message: "SPAWN_RESOURCE selected an unsupported bootstrap profile".to_owned(),
            }))
            .await;
        return;
    };
    let initial_size = usable_initial_size(request.initial_size);
    log_spawn_request(client_id, request_id, &request, initial_size);
    let SpawnRequest {
        group,
        command,
        cwd,
        env,
        term,
        satellite,
        owner_terminal,
        agent_session,
        resource,
        ..
    } = request;

    // An unrecognised kind is refused, never spawned as a Terminal.
    let kind = resource
        .as_ref()
        .map_or(phux_protocol::ids::ResourceKind::Terminal, |r| r.kind);
    let bind_instance = resource.as_ref().is_some_and(|r| r.bind_instance);
    let attribution = crate::runtime::commands::SpawnAttribution {
        actor: Some(client_id),
        operation_id: resource.as_ref().and_then(|r| r.idempotency_key),
        retain_secs: state.with(|s| {
            s.retain_policy()
                .resolve(resource.as_ref().and_then(|r| r.retain_secs))
        }),
    };
    match crate::resource::core_kind(kind) {
        Some(crate::resource::ResourceKind::Terminal) => {}
        Some(crate::resource::ResourceKind::AgentSession) => {
            let resource = resource.unwrap_or_default();
            crate::runtime::apps::agent_session::spawn::spawn_agent_session(
                state,
                client_id,
                request_id,
                &resource,
                satellite.as_ref(),
                out_tx,
                root_token,
                bootstrap_limits,
                connection_token,
            )
            .await;
            return;
        }
        _ => {
            warn!(
                ?client_id,
                request_id,
                ?kind,
                "SPAWN_RESOURCE: unknown kind"
            );
            refuse_spawn(out_tx, request_id, SpawnError::UnsupportedKind).await;
            return;
        }
    }
    // A Terminal takes no parent (ADR-0104 §5).
    if resource.as_ref().is_some_and(|r| r.parent.is_some()) {
        refuse_spawn(out_tx, request_id, SpawnError::ParentKindMismatch).await;
        return;
    }

    // The satellite validates the rest and its errors relay back verbatim.
    if let Some(host) = satellite {
        let spawn = satellite_spawn_owner(&host, owner_terminal, agent_session.is_some()).map(
            |owner_terminal| SatelliteSpawn {
                group,
                command,
                cwd,
                env,
                term,
                owner_terminal,
                initial_size,
                resource: forwarded_resource(
                    bind_instance,
                    attribution.operation_id,
                    resource.as_ref().and_then(|r| r.retain_secs),
                ),
            },
        );
        dispatch_satellite_spawn(state, client_id, out_tx, request_id, &host, spawn).await;
        return;
    }

    if let Err(refusal) = validate_local_spawn(agent_session.as_deref(), group) {
        refuse_spawn(out_tx, request_id, refusal).await;
        return;
    }

    let builder = build_spawn_command(state, client_id, command, cwd, env, term.as_deref()).await;

    let ownership = match resolve_spawn_ownership(state, client_id, owner_terminal) {
        Ok(ownership) => ownership,
        Err(refusal) => {
            refuse_spawn(out_tx, request_id, refusal).await;
            return;
        }
    };

    let (scrollback, default_colors) = state.with(|s| {
        (
            s.scrollback_limits(),
            s.attached()
                .get(&client_id)
                .and_then(|client| client.client_caps.default_colors),
        )
    });
    let core_terminal_id = match spawn_pane_or_refusal(
        state,
        client_id,
        request_id,
        &ownership,
        root_token,
        PaneSpawnPlan {
            builder,
            scrollback,
            default_colors,
            agent_session,
            initial_size,
            attribution,
        },
    ) {
        Ok(id) => id,
        Err(refusal) => {
            refuse_spawn(out_tx, request_id, refusal).await;
            return;
        }
    };

    let Some((wire_terminal_id, handle, client_caps)) =
        subscribe_spawning_client(state, client_id, core_terminal_id, out_tx)
    else {
        refuse_vanished_pane_handle(state, out_tx, client_id, request_id, core_terminal_id).await;
        return;
    };
    let terminal = match handle.terminal() {
        Ok(terminal) => terminal.clone(),
        Err(error) => {
            warn!(?client_id, request_id, ?core_terminal_id, %error, "SPAWN_RESOURCE");
            refuse_vanished_pane_handle(state, out_tx, client_id, request_id, core_terminal_id)
                .await;
            return;
        }
    };

    // ADR-0126: bind the key to the registered pane before anything awaits,
    // so a repeat never observes the pane without its binding.
    state.with(|s| {
        crate::runtime::idempotent_create::bind_spawned(
            s,
            attribution.operation_id,
            &wire_terminal_id,
        );
    });
    SpawnPublication {
        state,
        out_tx,
        request_id,
        client_id,
        core_terminal_id,
        wire_terminal_id,
        handle,
        terminal,
        client_caps,
        stream_id: stream_id_from(u64::from(request_id)),
        profile,
        limits: bootstrap_limits,
        bind_instance,
        idempotency_key: attribution.operation_id,
    }
    .publish(output_pumps, connection_token, defer_subscription)
    .await;
}
