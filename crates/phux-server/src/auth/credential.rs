//! Credential records, summaries, and the identities an admission carries.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use phux_protocol::policy::PeerIdentity;

#[cfg(doc)]
use super::prune_unused;
use super::{ReloadingTokenStore, STORE_VERSION};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CredentialFile {
    pub(super) version: u32,
    pub(super) credentials: Vec<CredentialRecord>,
}

impl Default for CredentialFile {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            credentials: Vec::new(),
        }
    }
}

/// One version of a credential. Rotation retains the prior generation for a
/// bounded overlap, so records are keyed by `(id, generation)` rather than id.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CredentialRecord {
    pub(super) id: String,
    pub(super) verifier: String,
    pub(super) principal: String,
    pub(super) scopes: Vec<String>,
    pub(super) issued_at: DateTime<Utc>,
    pub(super) expires_at: Option<DateTime<Utc>>,
    pub(super) revoked_at: Option<DateTime<Utc>>,
    pub(super) generation: u64,
    /// Last successful authentication of this generation; `None` if never
    /// used (or an older store).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) last_seen: Option<DateTime<Utc>>,
}

/// One credential as listed by `phux pair ls`: identity, mint time, last
/// successful authentication, and whether every generation is revoked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialSummary {
    /// Stable credential identifier shared by its rotated generations.
    pub id: String,
    /// Earliest `issued_at` across generations (when the credential was minted).
    pub issued_at: DateTime<Utc>,
    /// Latest `last_seen` across generations, when any authentication recorded one.
    pub last_seen: Option<DateTime<Utc>>,
    /// True when every generation carries a `revoked_at`.
    pub revoked: bool,
    /// Highest generation number currently on disk for this id.
    pub generation: u64,
}

/// Outcome of [`prune_unused`]: which credential ids were revoked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PruneOutcome {
    /// Credential ids revoked by this prune.
    pub revoked_ids: Vec<String>,
    pub(super) durable: bool,
}

impl PruneOutcome {
    /// Whether the store directory reached stable storage after the rewrite.
    #[must_use]
    pub const fn is_durable(&self) -> bool {
        self.durable
    }
}

/// Identity and policy captured at connection establishment. Revocation
/// and expiry still end the connection live (`workload-auth.md` §7), via
/// [`BearerAdmission`] and the grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedCredential {
    /// Stable credential identifier shared by its rotated generations.
    pub id: String,
    /// Authority principal represented by this credential.
    pub principal: String,
    /// Declared authorization scopes; enforcement belongs to the caller.
    pub scopes: Vec<String>,
    /// Time this generation was issued.
    pub issued_at: DateTime<Utc>,
    /// Optional time after which new authentication fails.
    pub expires_at: Option<DateTime<Utc>>,
    /// Monotonic generation within the credential identifier. For a workload
    /// credential, the generation of the registry snapshot that admitted it.
    pub generation: u64,
    /// The admitting registry file's instance id (workload credentials);
    /// key generation-derived state on `(registry_instance, generation)`.
    pub registry_instance: Option<String>,
}

/// Transport identity plus the credential attestation from establishment
/// (absent for local and SSH trust paths), retained for the connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionIdentity {
    /// Existing transport identity used by current terminal policy.
    pub peer: PeerIdentity,
    /// Structured remote credential, when bearer authentication was used.
    pub credential: Option<AuthenticatedCredential>,
    /// The ssh endpoints a same-uid `phux stdio-bridge` announced
    /// (`docs/spec/L3.md` §3.9); relabels the whoami route only, never read
    /// by policy.
    pub ssh_origin: Option<phux_protocol::wire::ssh_origin::SshOrigin>,
    /// The pairing credential that admitted the connection, so its
    /// revocation or expiry ends it live; `None` for local, loopback, and
    /// `OPEN_LISTENER` connections.
    pub bearer: Option<BearerAdmission>,
}

impl From<PeerIdentity> for ConnectionIdentity {
    fn from(peer: PeerIdentity) -> Self {
        Self {
            peer,
            credential: None,
            ssh_origin: None,
            bearer: None,
        }
    }
}

impl std::ops::Deref for ConnectionIdentity {
    type Target = PeerIdentity;

    fn deref(&self) -> &Self::Target {
        &self.peer
    }
}

/// The pairing credential that admitted a connection (§7).
#[derive(Clone, Debug)]
pub struct BearerAdmission {
    store: std::sync::Arc<ReloadingTokenStore>,
    /// The credential id.
    pub id: String,
    /// The credential generation that authenticated.
    pub generation: u64,
}

impl BearerAdmission {
    /// Record that `credential` was admitted from `store`.
    #[must_use]
    pub fn new(
        store: std::sync::Arc<ReloadingTokenStore>,
        credential: &AuthenticatedCredential,
    ) -> Self {
        Self {
            store,
            id: credential.id.clone(),
            generation: credential.generation,
        }
    }

    /// The token store that admitted the connection.
    #[must_use]
    pub(crate) const fn store(&self) -> &std::sync::Arc<ReloadingTokenStore> {
        &self.store
    }

    /// The credential's standing in the store's current generation, or
    /// `None` when the store gives no verdict (see
    /// [`ReloadingTokenStore::watch_snapshot`]).
    #[must_use]
    pub fn standing(&self) -> Option<Standing> {
        self.store.standing(&self.id, self.generation)
    }
}

impl PartialEq for BearerAdmission {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.store, &other.store)
            && self.id == other.id
            && self.generation == other.generation
    }
}

impl Eq for BearerAdmission {}

/// A bearer credential generation's standing in one store snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Admissible, until `expires_at` when set.
    Active {
        /// When the generation stops being admissible.
        expires_at: Option<DateTime<Utc>>,
    },
    /// Revoked, or absent from a store that loaded cleanly and holds
    /// credentials.
    Revoked,
    /// Past its expiry.
    Expired,
}

/// A newly minted bearer secret and its non-secret identity.
pub struct MintedCredential {
    /// Stable identifier of the credential.
    pub id: String,
    /// Newly minted generation number.
    pub generation: u64,
    pub(super) secret: String,
    pub(super) durable: bool,
}

impl MintedCredential {
    /// The bearer secret, exposed only for one-time delivery to the consumer.
    #[must_use]
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// Whether the directory was synced after rename. `false`: the
    /// credential is live but a crash could lose it; do not retry.
    #[must_use]
    pub const fn is_durable(&self) -> bool {
        self.durable
    }
}

impl std::fmt::Debug for MintedCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MintedCredential")
            .field("id", &self.id)
            .field("generation", &self.generation)
            .field("secret", &"[REDACTED]")
            .field("durable", &self.durable)
            .finish()
    }
}
