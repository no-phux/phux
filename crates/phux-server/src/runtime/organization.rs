//! Organization commands (ADR-0161, `docs/organization.md`). Mutations
//! commit through the organization store and answer `Ok` with a
//! [`Committed`] receipt, `Accepted` when effects follow as events, or
//! `Refused` (only these commands ever get `Refused` or `Accepted`, R08).
//! Effects start after the commit and report real outcomes as
//! `ActivationChanged`. A live termination starts under the commit lock
//! using the cascade checked before persistence.

#![expect(
    clippy::redundant_pub_crate,
    reason = "private organization modules re-export crate-scoped handlers through this facade; pub would violate unreachable_pub"
)]

use std::time::Duration;

#[cfg(doc)]
use phux_protocol::wire::frame::Committed;
use phux_protocol::wire::frame::{Command, CommandResult};
use tokio_util::sync::CancellationToken;

use crate::state::{ClientId, SharedState};

mod answer;
pub(crate) mod effects;
pub(in crate::runtime) mod launch_admission;
mod read;
mod translate;

pub(crate) use self::answer::answer;
use self::answer::lookup_creation;
pub(crate) use self::effects::run_effect;
use self::read::{arrangement_get, organization_page};
pub(crate) use self::translate::translate;
use crate::state::org_operation::{mutate, session_kill};

/// Records per `GET_ORGANIZATION` page when the caller asks for the
/// default.
pub(super) const DEFAULT_PAGE_SIZE: u32 = 100;

/// Most records per page.
pub(super) const MAX_PAGE_SIZE: u32 = 1000;

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
        Command::GetCreation { operation, session } => {
            state.with_mut(|s| lookup_creation(s, operation, session))
        }
        Command::SessionKill { operation, session } => {
            session_kill(state, client_id, &operation, &session, root_token)
        }
        other => mutate(state, client_id, other, root_token),
    }
}

#[cfg(test)]
mod tests;
