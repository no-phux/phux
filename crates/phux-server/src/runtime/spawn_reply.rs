//! Answering `SPAWN_RESOURCE`, for any kind: the typed reply, and relaying a
//! satellite-addressed spawn over the hub link.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared across the runtime; pub would violate unreachable_pub"
)]

use phux_protocol::wire::frame::{FrameKind, SpawnError, SpawnResult};
use tracing::debug;

use crate::hub::relay::SatelliteSpawn;
use crate::state::{ClientId, Outbound, SharedState};

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
