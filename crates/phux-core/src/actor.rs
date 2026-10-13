//! [`Actor`]: who issued an action. Every committed change records one.

use serde::{Deserialize, Serialize};

/// An authenticated identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalId {
    /// The serving user (ADR-0106). Holds every verb on its authority.
    Owner,
    /// An owner-registered agent or automation credential, by name.
    Credential(String),
    /// The server itself: executor reports and reconciliation.
    System,
}

impl PrincipalId {
    /// Whether this principal is a human. Only the owner is.
    #[must_use]
    pub const fn is_human(&self) -> bool {
        matches!(self, Self::Owner)
    }
}

/// Who issued an action: a principal, the client it spoke through, and the
/// human whose grant a delegated principal exercises.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Actor {
    /// The authenticated principal.
    pub principal: PrincipalId,
    /// The client name the principal spoke through, if any.
    #[serde(default)]
    pub client: Option<String>,
    /// The principal whose grant a delegated credential exercises.
    #[serde(default)]
    pub on_behalf_of: Option<PrincipalId>,
}

impl Actor {
    /// The owner, through no named client.
    #[must_use]
    pub const fn owner() -> Self {
        Self {
            principal: PrincipalId::Owner,
            client: None,
            on_behalf_of: None,
        }
    }

    /// The server itself.
    #[must_use]
    pub const fn system() -> Self {
        Self {
            principal: PrincipalId::System,
            client: None,
            on_behalf_of: None,
        }
    }
}

/// Permission category a command needs (CONTRACTS §03). Grants are
/// enforced by the server; core names the verb in refusals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verb {
    /// Read records and evidence.
    Observe,
    /// Typed input, paste, resize, kind actions.
    Interact,
    /// Rename, move membership, edit arrangements.
    Organize,
    /// Create records and launch execution.
    Create,
    /// Terminate, remove, delete.
    Terminate,
}
