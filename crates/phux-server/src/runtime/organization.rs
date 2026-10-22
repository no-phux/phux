//! Organization commands (ADR-0161, `docs/organization.md`): every
//! mutating one commits through [`ServerState::commit_command_from`] and
//! answers `Ok` with a [`Committed`] receipt, `Accepted` when execution
//! effects follow as events, or `Refused` with a typed reason. Reads
//! (`GET_ORGANIZATION`, `ARRANGEMENT_GET`) are answered from the records.
//!
//! Only these commands ever receive `Refused` or `Accepted` (R08).
//!
//! Effects run after the commit, off its lock: a launch starts the
//! Terminal's engine and reports its activation; a termination ends a live
//! engine through the same teardown as a legacy kill (graceful: hangup, the
//! grace, then kill; forced: the process group is killed at once), or
//! signals a lost process group after rechecking its identity. Both report
//! the real outcome as `ActivationChanged`.

use std::time::Duration;

use phux_core::Refusal;
#[cfg(doc)]
use phux_protocol::wire::frame::Committed;
use phux_protocol::wire::frame::{Command, CommandResult};
use tokio_util::sync::CancellationToken;

use crate::org_wire as ow;
use crate::state::{ClientId, ServerState, SharedState};

mod effects;
mod mutate;
mod read;
mod translate;

use self::effects::run_effect;
use self::mutate::{mutate, session_kill};
use self::read::{arrangement_get, organization_page};
use self::translate::{answer, translate};

/// Records per `GET_ORGANIZATION` page when the caller asks for the
/// default.
pub(super) const DEFAULT_PAGE_SIZE: u32 = 100;

/// Most records per page.
pub(super) const MAX_PAGE_SIZE: u32 = 1000;

/// Recognize only the reserved structured recipe, never a command/binary name.
/// Imported dormant records need no live wire handle to launch natively.
pub(super) fn native_relaunch_spec(
    s: &ServerState,
    resource: &phux_protocol::ids::ResourceKey,
    expected: phux_protocol::ids::Revision,
) -> Result<Option<phux_protocol::native_agent::SpawnNativeAgent>, CommandResult> {
    use phux_protocol::native_agent::{
        MAX_NATIVE_PAYLOAD_BYTES, NATIVE_HOST_SPEC_ENV, SpawnNativeAgent,
    };
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

/// How long a lost process group has to end after its termination signal
/// before it is killed.
const ORPHAN_GRACE: Duration = Duration::from_millis(500);

/// Answer one organization command.
pub(super) fn handle(
    state: &SharedState,
    client_id: ClientId,
    command: Command,
    root_token: &CancellationToken,
) -> CommandResult {
    match command {
        Command::GetOrganization {
            filter,
            page_size,
            cursor,
        } => state
            .with_mut(|s| organization_page(s, client_id, filter, page_size, cursor.as_deref())),
        Command::ArrangementGet { arrangement } => state.with(|s| arrangement_get(s, &arrangement)),
        Command::SessionKill { operation, session } => {
            session_kill(state, client_id, &operation, &session, root_token)
        }
        other => {
            let Some((operation, op)) = state.with(|s| translate(s, other)) else {
                return CommandResult::Error {
                    code: phux_protocol::wire::frame::ErrorCode::InvalidCommand,
                    message: "not an organization command".to_owned(),
                };
            };
            let op = match op {
                Ok(op) => op,
                Err(refusal) => return CommandResult::Refused(ow::refusal(&refusal)),
            };
            mutate(state, client_id, operation, op, root_token)
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]
mod tests;
