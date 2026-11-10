//! `SPAWN_RESOURCE` handling, including satellite relay.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;
use crate::runtime::apps::ResourceSpawn;
use crate::runtime::apps::spawn::SpawnLaunch;
use phux_terminal_api::actor::TerminalResource as _;

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

/// Serve a Terminal `SPAWN_RESOURCE` (SPEC §10.1): spawn a PTY-backed
/// Terminal into the spawning client's current session (or the most recently
/// active one for a detached spawner), auto-subscribe the spawner, and
/// publish its first generation. Refusals ride the typed `RESOURCE_SPAWNED`
/// reply. Satellite spawns are relayed over the hub link and never touch
/// local dispatch.
#[allow(
    clippy::too_many_lines,
    reason = "linear orchestration, one named helper per stage"
)]
pub(crate) async fn handle_spawn_terminal(spawn: ResourceSpawn<'_>) {
    let ResourceSpawn {
        state,
        client_id,
        request_id,
        resource,
        satellite,
        launch,
        out_tx,
        root_token,
        profile,
        limits: bootstrap_limits,
        connection_token,
        output_pumps,
        defer_subscription,
        decodes,
    } = spawn;
    let SpawnLaunch {
        group,
        command,
        cwd,
        env,
        term,
        owner_terminal,
        agent_session,
        initial_size,
    } = launch;
    let initial_size = usable_initial_size(initial_size);
    let bind_instance = resource.bind_instance;
    let attribution = crate::runtime::commands::SpawnAttribution {
        actor: Some(client_id),
        operation_id: resource.idempotency_key,
        retain_secs: state.with(|s| s.retain_policy().resolve(resource.retain_secs)),
    };
    // The spawner consumes the new Terminal's stream.
    if !decodes.contains(phux_protocol::ids::ResourceKind::Terminal) {
        let _ = out_tx
            .send(Outbound::Frame(FrameKind::Error {
                request_id: Some(request_id),
                code: ErrorCode::CodecUnavailable,
                message: "this connection's HELLO does not list the terminal resource kind, so it cannot consume its stream".to_owned(),
            }))
            .await;
        return;
    }
    // A Terminal takes no parent (ADR-0104 §5).
    if resource.parent.is_some() {
        refuse_spawn(out_tx, request_id, SpawnError::ParentKindMismatch).await;
        return;
    }

    // The satellite validates the rest and its errors relay back verbatim.
    if let Some(host) = satellite {
        let spawn = satellite_spawn_owner(host, owner_terminal, agent_session.is_some()).map(
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
                    resource.retain_secs,
                ),
            },
        );
        dispatch_satellite_spawn(state, client_id, out_tx, request_id, host, spawn).await;
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
