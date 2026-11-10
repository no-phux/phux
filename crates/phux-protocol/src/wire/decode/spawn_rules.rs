//! `SPAWN_RESOURCE` field absorption and its per-kind field rules.

use super::{
    DecodeError, Decoder, FrameKind, ResourceKind, SpawnResource, decode_flag,
    decode_idempotency_key, decode_terminal_id, field, utf8_value,
};
use crate::kinds::{ParentRule, Presence, SpawnContract, SpawnFacet};

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
        // The kind is not known yet: bound by the longest any kind takes,
        // and `validate_spawn_for_kind` holds each kind to its own.
        f::PROVIDER => {
            resource.provider = Some(decode_agent_facet_str(
                value,
                crate::kinds::max_spawn_provider_bytes(),
            )?);
        }
        f::NATIVE_ID => {
            resource.native_id = Some(decode_agent_facet_str(
                value,
                crate::kinds::max_spawn_native_id_bytes(),
            )?);
        }
        f::BIND_INSTANCE => resource.bind_instance = sub!(value, decode_bind_instance),
        f::RETAIN_SECS => resource.retain_secs = Some(sub!(value, Decoder::read_u32_be)),
        f::IDEMPOTENCY_KEY => resource.idempotency_key = Some(decode_idempotency_key(value)?),
        f::CONTROL => {
            resource.control =
                crate::wire::frame::ControlClass::from_u8(sub!(value, Decoder::read_u8));
        }
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

/// Enforce `SPAWN_RESOURCE`'s per-kind field rules, the kind's catalog
/// [`SpawnContract`] (`docs/protocol.md#commands-events-resources`).
///
/// A kind this build does not serve passes unvalidated so the server can
/// answer `UnsupportedKind` rather than fail the connection.
pub(super) fn validate_spawn_for_kind(frame: &FrameKind) -> Result<(), DecodeError> {
    validate_spawn_for_kind_in(&crate::kinds::KINDS, frame)
}

/// [`validate_spawn_for_kind`] under `catalog`.
pub(super) fn validate_spawn_for_kind_in(
    catalog: &'static [crate::kinds::KindSpec],
    frame: &FrameKind,
) -> Result<(), DecodeError> {
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
    let Some(spec) = crate::kinds::kind_spec_in(catalog, resource.kind) else {
        return Ok(());
    };
    let contract = &spec.spawn;
    let longer = |value: &Option<String>, facet: SpawnFacet| {
        value.as_ref().is_some_and(|v| v.len() > facet.max_bytes)
    };
    if longer(&resource.provider, contract.provider)
        || longer(&resource.native_id, contract.native_id)
    {
        return Err(DecodeError::AgentFacetLimitExceeded);
    }
    let present = |id: u32| match id {
        field::spawn_terminal::COMMAND => command.is_some(),
        field::spawn_terminal::CWD => cwd.is_some(),
        field::spawn_terminal::ENV => env.is_some(),
        field::spawn_terminal::TERM => term.is_some(),
        field::spawn_terminal::OWNER_TERMINAL => owner_terminal.is_some(),
        field::spawn_terminal::INITIAL_SIZE => initial_size.is_some(),
        field::spawn_terminal::PARENT => resource.parent.is_some(),
        field::spawn_terminal::PROVIDER => resource.provider.is_some(),
        field::spawn_terminal::NATIVE_ID => resource.native_id.is_some(),
        field::spawn_terminal::RETAIN_SECS => resource.retain_secs.is_some(),
        field::spawn_terminal::CONTROL => resource.control.is_some(),
        _ => false,
    };
    let refused = contract_violation(contract, present).or_else(|| {
        contract
            .validate
            .and_then(|validate| validate(resource).err())
            .map(|field| (field, true))
    });
    match refused {
        Some((field, required)) => Err(DecodeError::InvalidSpawnForKind {
            kind: resource.kind.as_wire(),
            field,
            required,
        }),
        None => Ok(()),
    }
}

/// The first field `contract` refuses, in its check order, with whether it
/// was required (absent) or forbidden (present).
fn contract_violation(
    contract: &SpawnContract,
    present: impl Fn(u32) -> bool,
) -> Option<(u32, bool)> {
    let parent = match contract.parent {
        ParentRule::Required => Presence::Required,
        ParentRule::Forbidden => Presence::Forbidden,
    };
    let facets = [
        (field::spawn_terminal::PARENT, parent),
        (field::spawn_terminal::PROVIDER, contract.provider.presence),
        (
            field::spawn_terminal::NATIVE_ID,
            contract.native_id.presence,
        ),
    ];
    let forbidden = contract
        .forbidden
        .iter()
        .map(|id| (*id, Presence::Forbidden));
    facets
        .into_iter()
        .chain(forbidden)
        .find_map(|(id, presence)| match (presence, present(id)) {
            (Presence::Required, false) => Some((id, true)),
            (Presence::Forbidden, true) => Some((id, false)),
            _ => None,
        })
}
