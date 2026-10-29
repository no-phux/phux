//! `SPAWN_RESOURCE` field absorption and its per-kind field rules.

use super::{
    DecodeError, Decoder, FrameKind, MAX_RESOURCE_NATIVE_ID_BYTES, MAX_RESOURCE_PROVIDER_BYTES,
    ResourceKind, SpawnResource, decode_flag, decode_idempotency_key, decode_terminal_id, field,
    utf8_value,
};

/// Take one of `SPAWN_RESOURCE`'s resource fields (11-17); ignore other ids.
pub(super) fn absorb_spawn_resource_field(
    resource: &mut SpawnResource,
    id: u32,
    value: &[u8],
) -> Result<(), DecodeError> {
    use field::spawn_terminal as f;
    match id {
        f::KIND => resource.kind = ResourceKind::from_wire(sub!(value, Decoder::read_u8)),
        f::PARENT => resource.parent = Some(sub!(value, decode_terminal_id)),
        f::PROVIDER => {
            resource.provider = Some(decode_agent_facet_str(value, MAX_RESOURCE_PROVIDER_BYTES)?);
        }
        f::NATIVE_ID => {
            resource.native_id = Some(decode_agent_facet_str(value, MAX_RESOURCE_NATIVE_ID_BYTES)?);
        }
        f::BIND_INSTANCE => resource.bind_instance = sub!(value, decode_bind_instance),
        f::RETAIN_SECS => resource.retain_secs = Some(sub!(value, Decoder::read_u32_be)),
        f::IDEMPOTENCY_KEY => resource.idempotency_key = Some(decode_idempotency_key(value)?),
        _ => {}
    }
    Ok(())
}

/// Read `SPAWN_RESOURCE.bind_instance` (field 15): `0` or `1`.
pub(super) fn decode_bind_instance(dec: &mut Decoder<'_>) -> Result<bool, DecodeError> {
    decode_flag(dec, "SpawnResource.bind_instance")
}

/// Fold `RESOURCE_SPAWNED.instance` and `replayed` into the typed result;
/// both are dropped beside a refusal.
pub(super) fn bind_spawn_result(
    result: crate::wire::frame::SpawnResult,
    instance: Option<crate::ids::ServerInstance>,
    replayed: bool,
) -> crate::wire::frame::SpawnResult {
    use crate::wire::frame::SpawnResult;
    match (result, instance) {
        (SpawnResult::Ok(id), instance) if replayed => SpawnResult::Replayed { id, instance },
        (SpawnResult::Ok(id), Some(instance)) => SpawnResult::OkBound { id, instance },
        (result, _) => result,
    }
}

/// Decode an agent-facet string, refusing empty or over-`max_bytes` values
/// before copying.
pub(super) fn decode_agent_facet_str(
    value: &[u8],
    max_bytes: usize,
) -> Result<String, DecodeError> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(DecodeError::AgentFacetLimitExceeded);
    }
    utf8_value(value)
}

/// Enforce `SPAWN_RESOURCE`'s per-kind field rules (`docs/protocol.md#commands-events-resources`).
///
/// An `Unknown` kind passes unvalidated so the server can answer
/// `UnsupportedKind` rather than fail the connection.
pub(super) fn validate_spawn_for_kind(frame: &FrameKind) -> Result<(), DecodeError> {
    let FrameKind::SpawnResource {
        command,
        cwd,
        env,
        term,
        owner_terminal,
        initial_size,
        resource,
        ..
    } = frame
    else {
        return Ok(());
    };
    let Some(resource) = resource.as_deref() else {
        return Ok(());
    };
    let SpawnResource {
        kind,
        parent,
        provider,
        native_id,
        retain_secs,
        ..
    } = resource;
    let rule = |field: u32, required: bool| DecodeError::InvalidSpawnForKind {
        kind: kind.as_wire(),
        field,
        required,
    };
    match kind {
        ResourceKind::Terminal => {
            for (field, present) in [
                (field::spawn_terminal::PARENT, parent.is_some()),
                (field::spawn_terminal::PROVIDER, provider.is_some()),
                (field::spawn_terminal::NATIVE_ID, native_id.is_some()),
            ] {
                if present {
                    return Err(rule(field, false));
                }
            }
        }
        ResourceKind::AgentSession => {
            for (field, present) in [
                (field::spawn_terminal::PARENT, parent.is_some()),
                (field::spawn_terminal::PROVIDER, provider.is_some()),
            ] {
                if !present {
                    return Err(rule(field, true));
                }
            }
            for (field, present) in [
                (field::spawn_terminal::COMMAND, command.is_some()),
                (field::spawn_terminal::CWD, cwd.is_some()),
                (field::spawn_terminal::ENV, env.is_some()),
                (field::spawn_terminal::TERM, term.is_some()),
                (
                    field::spawn_terminal::OWNER_TERMINAL,
                    owner_terminal.is_some(),
                ),
                (field::spawn_terminal::INITIAL_SIZE, initial_size.is_some()),
                (field::spawn_terminal::RETAIN_SECS, retain_secs.is_some()),
            ] {
                if present {
                    return Err(rule(field, false));
                }
            }
        }
        ResourceKind::Unknown { .. } => {}
    }
    Ok(())
}
