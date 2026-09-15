//! Idempotent creates (ADR-0126, SPEC L1 §3.1, L3 §3.1): a keyed
//! `SPAWN_RESOURCE` and a token-bearing `phux.session.create/v1`, both on the
//! server's shared dedupe record ([`super::operation_dedupe`]).
//!
//! A keyed spawn is admitted before it runs. The first admission owns the
//! key and spawns; the spawn binds the key to the resource in the step that
//! registers it, before anything can await, so no repeat ever observes the
//! resource without its binding. A repeat with the same payload answers the
//! bound resource marked replayed and runs nothing: no placement, no
//! agent-session record, no second `pane_spawned`. A repeat with another
//! payload is `IDEMPOTENCY_CONFLICT`. A spawn that failed binds nothing, and
//! the next repeat spawns.
//!
//! A satellite-addressed spawn is not evaluated here: the hub forwards the
//! key and the satellite that creates the resource owns its dedupe.

use std::time::{Duration, Instant};

use bytes::BytesMut;
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile};
use phux_protocol::ids::{IdempotencyKey, ResourceId as WireResourceId};
use phux_protocol::wire::frame::{FrameKind, SpawnError, SpawnResult};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::attach::{SpawnRequest, handle_spawn_terminal};
use super::operation_dedupe::{
    CachedOutcome, Claim, OperationClaim, OperationDomain, OperationKey, Waiter,
};
use crate::state::{ClientId, Outbound, ServerState, SharedState};

/// Handle `SPAWN_RESOURCE`, honoring its idempotency key (field 17).
///
/// An unkeyed spawn, and a satellite-addressed one, goes straight to
/// [`handle_spawn_terminal`].
#[allow(
    clippy::too_many_arguments,
    reason = "the handle_spawn_terminal argument list, passed through unchanged"
)]
pub(crate) async fn handle_spawn_resource(
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
) {
    let claim = match admit_keyed_spawn(state, &request, REPEAT_WAIT).await {
        SpawnAdmission::Unkeyed => None,
        SpawnAdmission::Owner(claim) => Some(claim),
        SpawnAdmission::Answer(result) => {
            let _ = out_tx
                .send(Outbound::Frame(FrameKind::ResourceSpawned {
                    request_id,
                    result,
                }))
                .await;
            return;
        }
    };
    handle_spawn_terminal(
        state,
        client_id,
        request_id,
        request,
        out_tx,
        bootstrap_profile,
        bootstrap_limits,
        root_token,
        connection_token,
        output_pumps,
    )
    .await;
    // The spawn bound its key when it registered the resource, or bound
    // nothing; dropping the claim releases a key that is still unbound.
    drop(claim);
}

/// Bind a keyed spawn's key to the resource it just registered. Call it in
/// the step that registers the resource, before any await. A spawn that
/// carried no key binds nothing.
pub(crate) fn bind_spawned(s: &ServerState, key: Option<IdempotencyKey>, id: &WireResourceId) {
    let Some(key) = key else {
        return;
    };
    s.operation_dedupe()
        .set_final(spawn_key(key), &spawned_outcome(s, id));
}

/// Undo [`bind_spawned`] for a resource reaped before its spawner received
/// a usable generation: a refused spawn binds nothing, so a retry spawns
/// fresh (SPEC L1 §3.1). Call it in the step that reaps the resource. A key
/// that has since been bound to something else is left alone.
pub(crate) fn unbind_spawned(s: &ServerState, key: Option<IdempotencyKey>, id: &WireResourceId) {
    let Some(key) = key else {
        return;
    };
    s.operation_dedupe()
        .unbind(spawn_key(key), &spawned_outcome(s, id));
}

fn spawned_outcome(s: &ServerState, id: &WireResourceId) -> CachedOutcome {
    CachedOutcome::Spawn {
        id: id.clone(),
        instance: s.idspace.instance(),
    }
}

/// How long a repeat waits for the same key's unresolved spawn before it is
/// refused. The wait runs in the repeating connection's read loop.
const REPEAT_WAIT: Duration = Duration::from_secs(10);

enum SpawnAdmission {
    /// No key, or not this server's to evaluate.
    Unkeyed,
    /// This request runs the spawn.
    Owner(OperationClaim),
    /// This request is answered without spawning.
    Answer(SpawnResult),
}

/// The key of a spawn this server evaluates. A satellite-addressed spawn is
/// the satellite's to evaluate.
fn local_spawn_key(request: &SpawnRequest) -> Option<IdempotencyKey> {
    if request.satellite.is_some() {
        return None;
    }
    request.resource.as_ref().and_then(|r| r.idempotency_key)
}

async fn admit_keyed_spawn(
    state: &SharedState,
    request: &SpawnRequest,
    wait: Duration,
) -> SpawnAdmission {
    let Some(key) = local_spawn_key(request) else {
        return SpawnAdmission::Unkeyed;
    };
    let dedupe = state.with(|s| s.operation_dedupe().clone());
    let key = spawn_key(key);
    let digest = spawn_digest(request);
    let bind_instance = request.resource.as_ref().is_some_and(|r| r.bind_instance);
    loop {
        let pending = match dedupe.claim_at(key, digest, Instant::now(), join_spawn) {
            Claim::Owner => return SpawnAdmission::Owner(OperationClaim::new(dedupe, key)),
            Claim::Pending(pending) => pending,
            Claim::Final(outcome) => {
                return SpawnAdmission::Answer(replayed(&outcome, bind_instance));
            }
            Claim::PendingUncertain => {
                return SpawnAdmission::Answer(refused(
                    "a spawn under this idempotency key is still in flight; retry",
                ));
            }
            Claim::Conflict => {
                return SpawnAdmission::Answer(SpawnResult::Err(SpawnError::IdempotencyConflict));
            }
            Claim::Full => {
                return SpawnAdmission::Answer(refused(
                    "the server's idempotency record is full; retry later",
                ));
            }
        };
        match tokio::time::timeout(wait, pending).await {
            Ok(Ok(outcome)) => {
                return SpawnAdmission::Answer(replayed(&outcome, bind_instance));
            }
            // The owner bound nothing and released the key: admit again.
            Ok(Err(_)) => {}
            Err(_) => return SpawnAdmission::Answer(refused("operation in flight; retry")),
        }
    }
}

const fn spawn_key(key: IdempotencyKey) -> OperationKey {
    OperationKey::new(OperationDomain::Spawn, *key.as_bytes())
}

/// The digest a key is bound to: `SPAWN_RESOURCE` fields 2 through 16, the
/// whole request but its `request_id` and the key itself.
fn spawn_digest(request: &SpawnRequest) -> [u8; 32] {
    let resource = request.resource.as_deref().map(|resource| {
        let mut resource = resource.clone();
        resource.idempotency_key = None;
        Box::new(resource)
    });
    let frame = FrameKind::SpawnResource {
        request_id: 0,
        group: request.group,
        command: request.command.clone(),
        cwd: request.cwd.clone(),
        env: request.env.clone(),
        term: request.term.clone(),
        satellite: request.satellite.clone(),
        owner_terminal: request.owner_terminal.clone(),
        agent_session: request.agent_session.clone(),
        initial_size: request.initial_size,
        resource,
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    Sha256::digest(&encoded).into()
}

fn join_spawn() -> (Waiter, oneshot::Receiver<CachedOutcome>) {
    let (reply, outcome) = oneshot::channel();
    let waiter: Waiter = Box::new(move |bound: &CachedOutcome| {
        let _ = reply.send(bound.clone());
    });
    (waiter, outcome)
}

/// The reply to a repeat: the original id, marked replayed, with the
/// original instance token when the spawn asked for one. `bind_instance` is
/// part of the digest, so the repeat asked exactly as the original did.
fn replayed(outcome: &CachedOutcome, bind_instance: bool) -> SpawnResult {
    let CachedOutcome::Spawn { id, instance } = outcome else {
        return refused("the idempotency record holds no spawn under this key");
    };
    SpawnResult::Replayed {
        id: id.clone(),
        instance: bind_instance.then_some(*instance),
    }
}

fn refused(message: &str) -> SpawnResult {
    SpawnResult::Err(SpawnError::SpawnFailed(message.to_owned()))
}

/// How a token-bearing session create is admitted.
pub(crate) enum SessionCreateAdmission {
    /// No usable token: the create runs without dedupe, as it always did.
    Unkeyed,
    /// This request runs the create and binds its result.
    Owner(OperationClaim),
    /// A repeat inside the horizon: publish this result again.
    Replay(serde_json::Value),
    /// Refused without creating anything, for the logged reason.
    Refused(&'static str),
}

/// Admit a session create whose `request_token` is `token`, bound to the
/// request's `digest`.
pub(crate) fn admit_session_create(
    state: &SharedState,
    token: Option<[u8; 16]>,
    digest: [u8; 32],
) -> SessionCreateAdmission {
    let Some(token) = token else {
        return SessionCreateAdmission::Unkeyed;
    };
    let dedupe = state.with(|s| s.operation_dedupe().clone());
    let key = OperationKey::new(OperationDomain::SessionCreate, token);
    match dedupe.claim_at(key, digest, Instant::now(), join_nothing) {
        Claim::Owner => SessionCreateAdmission::Owner(OperationClaim::new(dedupe, key)),
        Claim::Final(CachedOutcome::SessionCreate(payload)) => {
            SessionCreateAdmission::Replay(payload)
        }
        Claim::Final(_) | Claim::Conflict => {
            SessionCreateAdmission::Refused("request token reused with a different request")
        }
        // A create runs within one server turn, so another is never seen
        // unresolved; refuse rather than wait on the impossible.
        Claim::Pending(()) | Claim::PendingUncertain => {
            SessionCreateAdmission::Refused("a create under this request token is in flight")
        }
        Claim::Full => SessionCreateAdmission::Refused("the server's idempotency record is full"),
    }
}

fn join_nothing() -> (Waiter, ()) {
    (Box::new(|_| {}), ())
}

/// The 16 bytes of a `request_token` UUID (`8-4-4-4-12` hex), or `None` when
/// it is not one.
pub(crate) fn uuid_bytes(token: &str) -> Option<[u8; 16]> {
    let hex: Vec<u8> = token.bytes().filter(|byte| *byte != b'-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0; 16];
    let (pairs, _) = hex.as_chunks::<2>();
    for (byte, pair) in bytes.iter_mut().zip(pairs) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn keyed(command: &str) -> SpawnRequest {
        SpawnRequest {
            group: crate::state::DEFAULT_GROUP_ID,
            command: Some(vec![command.to_owned()]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: Some(Box::new(
                phux_protocol::wire::frame::SpawnResource::default()
                    .with_idempotency_key(IdempotencyKey::new([4; 16])),
            )),
        }
    }

    #[test]
    fn the_digest_ignores_the_key_and_covers_the_payload() {
        let mut other_key = keyed("/bin/cat");
        if let Some(resource) = other_key.resource.as_mut() {
            resource.idempotency_key = IdempotencyKey::new([5; 16]);
        }
        assert_eq!(spawn_digest(&keyed("/bin/cat")), spawn_digest(&other_key));
        assert_ne!(
            spawn_digest(&keyed("/bin/cat")),
            spawn_digest(&keyed("/bin/sh"))
        );
    }

    #[test]
    fn a_satellite_spawn_is_not_evaluated_here() {
        let mut request = keyed("/bin/cat");
        assert!(local_spawn_key(&request).is_some());
        request.satellite = Some(phux_protocol::ids::SatelliteHost::new("devbox"));
        assert!(local_spawn_key(&request).is_none());
    }

    #[test]
    fn a_replay_carries_the_instance_only_when_the_spawn_bound_one() {
        let instance = phux_protocol::ids::ServerInstance::new([6; 16]);
        let outcome = CachedOutcome::Spawn {
            id: WireResourceId::local(3),
            instance,
        };
        assert_eq!(
            replayed(&outcome, false),
            SpawnResult::Replayed {
                id: WireResourceId::local(3),
                instance: None
            }
        );
        assert_eq!(
            replayed(&outcome, true),
            SpawnResult::Replayed {
                id: WireResourceId::local(3),
                instance: Some(instance)
            }
        );
    }

    #[test]
    fn request_tokens_parse_as_uuids() {
        assert_eq!(
            uuid_bytes("11111111-1111-4111-8111-111111111111").map(|b| b[0]),
            Some(0x11)
        );
        assert!(uuid_bytes("not-a-uuid").is_none());
    }

    /// A repeat behind an owner that never resolves is refused once the
    /// bound passes, instead of holding its connection's read loop.
    #[tokio::test]
    async fn a_repeat_behind_a_stalled_owner_is_refused_after_the_bound() {
        let state = SharedState::new();
        let request = keyed("/bin/cat");
        let dedupe = state.with(|s| s.operation_dedupe().clone());
        let key = spawn_key(IdempotencyKey::new([4; 16]).expect("non-zero key"));
        let Claim::Owner = dedupe.claim_at(key, spawn_digest(&request), Instant::now(), join_spawn)
        else {
            panic!("the stalled owner admits first");
        };
        let _stalled = OperationClaim::new(dedupe, key);
        let SpawnAdmission::Answer(SpawnResult::Err(SpawnError::SpawnFailed(message))) =
            admit_keyed_spawn(&state, &request, Duration::from_millis(20)).await
        else {
            panic!("the repeat is refused, not admitted");
        };
        assert!(message.contains("in flight"), "{message}");
    }

    /// A pane reaped before its spawner could use it takes its binding with
    /// it; a later binding of the same key to another pane survives.
    #[test]
    fn a_reaped_spawn_is_unbound_and_its_retry_spawns_fresh() {
        let state = SharedState::new();
        let request = keyed("/bin/cat");
        let key = IdempotencyKey::new([4; 16]);
        let operation = spawn_key(key.expect("non-zero key"));
        let dedupe = state.with(|s| s.operation_dedupe().clone());
        let digest = spawn_digest(&request);
        let owner = || match dedupe.claim_at(operation, digest, Instant::now(), join_spawn) {
            Claim::Owner => true,
            Claim::Final(_) => false,
            _ => panic!("unexpected admission"),
        };
        assert!(owner());
        state.with(|s| bind_spawned(s, key, &WireResourceId::local(3)));
        assert!(!owner(), "a bound key replays");
        state.with(|s| unbind_spawned(s, key, &WireResourceId::local(3)));
        assert!(owner(), "the reaped pane's retry spawns fresh");
        state.with(|s| bind_spawned(s, key, &WireResourceId::local(4)));
        state.with(|s| unbind_spawned(s, key, &WireResourceId::local(3)));
        assert!(!owner(), "an unbind naming another pane changes nothing");
    }
}
