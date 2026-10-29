//! The server's authorization (`docs/remote.md#who-can-do-what`, ADR-0116,
//! ADR-0125): one grant per connection, minted at HELLO, enforced at every
//! dispatch.
//!
//! [`Authority::Owner`] (all verbs at Global) is minted for the owner's
//! socket in both modes, so its handlers behave as before.
//! [`Authority::Scoped`] is a workload's registry ceiling, minted only in
//! `paired` from the live registry, never from bearer scopes. There is no
//! third mode (ADR-0179): nothing grants a remote peer the owner's authority.
//!
//! [`authorize_frame`], [`authorize_command`], and [`authorize_stream_bind`]
//! share [`enforce`] over [`phux_protocol::kinds`]'s classification.
//! [`ScopedPolicy`] is the engine of both modes; an embedder may inject its
//! own through `ServerConfig::policy_engine` (ADR-0072).

mod enforce;
mod engine;
pub mod filter;
mod grant;
mod hold;
mod posture;

#[cfg(test)]
mod tests;

use std::time::Duration;

use chrono::{DateTime, Utc};
#[cfg(test)]
use phux_protocol::policy::PeerIdentity;
use phux_protocol::wire::frame::DetachReason;

#[cfg(test)]
use crate::auth::AuthenticatedCredential;

pub use enforce::{
    Denial, Request, authorize_command, authorize_frame, authorize_stream_bind, enforce,
    resolve_attach_session,
};
pub use engine::{GrantFuture, PolicyEngine, PolicyError, ScopedPolicy};
pub use grant::{Authority, ConnectionGrant, GroupPoint, WORKLOAD_PRINCIPAL};
pub use hold::Admission;
pub use posture::{PolicyPosture, PostureError, PostureInputs, ResumeFallback};

/// The Terminals a batch command (`KILL_RESOURCES`, `CLOSE_TAB_RESOURCES`)
/// names; the guard, holds, and withdrawal all read this list.
#[must_use]
pub(crate) fn batch_terminals(
    command: &phux_protocol::wire::frame::Command,
) -> &[phux_protocol::ids::ResourceId] {
    use phux_protocol::wire::frame::Command;
    match command {
        Command::KillResources { ids, .. } | Command::CloseTabResources { ids } => ids,
        _ => &[],
    }
}

/// Every Terminal a held command names (ADR-0128).
pub(crate) fn held_terminals(
    command: &phux_protocol::wire::frame::Command,
) -> impl Iterator<Item = &phux_protocol::ids::ResourceId> {
    batch_terminals(command)
        .iter()
        .chain(enforce::named_terminal(Request::Command(command)))
}

/// The tracing target every authorization decision logs under.
pub const POLICY_TARGET: &str = "phux_server::policy";

/// At most one uncorrelated `PERMISSION_DENIED` per connection per interval
/// (§7); denied frames are still dropped.
pub const DENIAL_ERROR_INTERVAL: Duration = Duration::from_secs(1);

/// Why a live connection's authority was withdrawn (`docs/remote.md#managing-credentials`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revocation {
    /// The credential was removed or revoked, its store stopped loading, or
    /// the ceiling no longer contains the minted grant.
    Revoked,
    /// The credential reached its expiry.
    Expired,
}

impl Revocation {
    /// The `DETACHED` reason that ends the connection.
    #[must_use]
    pub const fn detach_reason(self) -> DetachReason {
        match self {
            Self::Revoked => DetachReason::AuthorizationRevoked,
            Self::Expired => DetachReason::AuthorizationExpired,
        }
    }

    /// The diagnostic text on the goodbye frames. Names no credential and
    /// no rule.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Revoked => "authorization revoked",
            Self::Expired => "authorization expired",
        }
    }
}

/// What a connection's writer owes the peer once its authority is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goodbye {
    /// `ERROR { PERMISSION_DENIED }`, then `DETACHED` with the revocation's
    /// reason, then close.
    Announce(Revocation),
    /// Close with no frame: the connection never completed HELLO, so there
    /// is no attach for a `DETACHED` to end (`docs/remote.md#managing-credentials`).
    Silent,
}

/// The registry state a live scoped grant was last confirmed against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryStamp {
    /// The registry file instance.
    pub instance: Option<String>,
    /// The registry generation.
    pub generation: u64,
    /// The credential's expiry in that state.
    pub expires_at: Option<DateTime<Utc>>,
}

/// One live connection as the revocation watcher sees it.
#[derive(Debug, Clone)]
pub(crate) struct WatchedConnection {
    /// The connection.
    pub(crate) client: crate::state::ClientId,
    /// Its grant, as it stood when the watcher looked.
    pub(crate) grant: ConnectionGrant,
    /// The pairing-store admission, if a bearer token admitted it.
    pub(crate) bearer: Option<crate::auth::BearerAdmission>,
}

/// An embedder engine (ADR-0072) for this crate's transport tests: every
/// connection holds the owner's grant, which no `phux server` mode gives a
/// remote peer (ADR-0179). Tests of authorization use [`ScopedPolicy`].
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct OwnerForAll;

#[cfg(test)]
impl PolicyEngine for OwnerForAll {
    fn authorize_hello<'a>(
        &'a self,
        _peer_identity: &'a PeerIdentity,
        _credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        Box::pin(async { Ok(ConnectionGrant::owner()) })
    }
}
