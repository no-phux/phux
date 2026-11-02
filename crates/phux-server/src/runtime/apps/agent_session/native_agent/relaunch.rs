//! Relaunching a saved native agent through the native launch transaction.

use phux_core::Refusal;
use phux_protocol::ids::{OperationId, ResourceKey, Revision};
use phux_protocol::native_agent::{
    MAX_NATIVE_PAYLOAD_BYTES, NATIVE_HOST_SPEC_ENV, SpawnNativeAgent,
};
use phux_protocol::wire::frame::CommandResult;

use super::launch::spawn_relaunch;
use crate::org_wire as ow;
use crate::runtime::commands::CommandContext;
use crate::state::ServerState;

/// Serve this relaunch if the resource's recipe is a native one.
pub(crate) fn claim_relaunch(
    ctx: &CommandContext,
    operation: OperationId,
    resource: &ResourceKey,
    expected: Revision,
) -> Option<CommandResult> {
    match ctx
        .state
        .with(|s| native_relaunch_spec(s, resource, expected))
    {
        Ok(Some(spec)) => Some(spawn_relaunch(ctx, spec, operation)),
        Ok(None) => None,
        Err(result) => Some(result),
    }
}

/// Recognize only the reserved structured recipe, never a command/binary name.
/// Imported dormant records need no live wire handle to launch natively.
fn native_relaunch_spec(
    s: &ServerState,
    resource: &phux_protocol::ids::ResourceKey,
    expected: phux_protocol::ids::Revision,
) -> Result<Option<phux_protocol::native_agent::SpawnNativeAgent>, CommandResult> {
    let Some(id) = s.registry().resource_id(&ow::core_resource_key(resource)) else {
        return Ok(None);
    };
    let Some(record) = s.registry().record(id) else {
        return Ok(None);
    };
    let Some(phux_core::LaunchSpec::Terminal(launch)) = &record.launch else {
        return Ok(None);
    };
    let mut markers = launch
        .env
        .iter()
        .filter(|(key, _)| key == NATIVE_HOST_SPEC_ENV);
    let Some((_, encoded)) = markers.next() else {
        return Ok(None);
    };
    let invalid = |reason: String| {
        CommandResult::Refused(ow::refusal(&Refusal::InvalidArgument {
            field: "native_recipe",
            reason,
        }))
    };
    if markers.next().is_some() || encoded.len() > MAX_NATIVE_PAYLOAD_BYTES {
        return Err(invalid(
            "duplicate or oversized native launch recipe".into(),
        ));
    }
    if record.revision != ow::core_revision(expected) {
        return Err(CommandResult::Refused(ow::refusal(&Refusal::Conflict {
            current: record.revision,
        })));
    }
    let mut spec: SpawnNativeAgent =
        serde_json::from_str(encoded).map_err(|error| invalid(error.to_string()))?;
    if spec.guardian.is_some()
        || spec.guardian_key.is_some()
        || spec.guardian_expected.is_some()
        || spec.env.keys().any(|key| {
            key.starts_with("PHUX_NATIVE_")
                || matches!(key.as_str(), "PHUX_SOCKET" | "PHUX_TERMINAL_ID")
        })
    {
        return Err(invalid(
            "saved native recipe contains private bootstrap or stale destination data".into(),
        ));
    }
    let session = s
        .registry()
        .session_of(id)
        .and_then(|session| s.registry().session(session))
        .ok_or_else(|| invalid("native guardian has no organization session".into()))?;
    spec.session.clone_from(&session.name);
    spec.guardian_key = Some(*resource);
    spec.guardian_expected = Some(expected);
    Ok(Some(spec))
}
