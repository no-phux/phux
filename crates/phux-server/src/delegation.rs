//! Delegated agent credentials (ADR-0163 §5, CONTRACTS §03).
//!
//! A launcher (the owner, or a principal already holding the verbs) mints a
//! credential an agent acts through: [`AGENT_VERBS`] or fewer, on the
//! launcher's session only, `on_behalf_of` the launcher's human, for a
//! bounded lifetime. Delegation only narrows: every verb must be one the
//! launcher holds on that whole session, and none may lie outside
//! [`AGENT_VERBS`], so `SIGNAL` (which still authorizes legacy kills, R07),
//! `TERMINATE`, `ORGANIZE`, `CREATE`, and `BIND` are never delegated.
//!
//! The ledger is in memory: a credential names a session by its wire id,
//! which restarts with the server, so it dies with the server too. It is not
//! a workload-registry record and is never written to disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use phux_core::PrincipalId;
use phux_core::ids::SessionId;
use phux_protocol::kinds::{Verb, Verbs};
use phux_protocol::scope::{ScopeGrant, Selector, TerminalScopeSet};

/// The verbs an agent credential may carry: read the session's records and
/// content, and type into it. Also the default request.
pub const AGENT_VERBS: Verbs = Verbs::of(&[Verb::Inventory, Verb::Observe, Verb::Input]);

/// The lifetime a request gets when it names none.
pub const DEFAULT_LIFETIME: Duration = Duration::from_hours(12);

/// The longest lifetime a delegated credential may have.
pub const MAX_LIFETIME: Duration = Duration::from_hours(24);

/// The prefix of every delegated credential id; no workload-registry id
/// (`sha256:`) or pairing id carries it.
pub const ID_PREFIX: &str = "delegated:";

/// What a launcher asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelegationRequest {
    /// The verbs to delegate; a subset of [`AGENT_VERBS`].
    pub verbs: Verbs,
    /// How long the credential lives; clamped to [`MAX_LIFETIME`] and to
    /// the launcher's own expiry.
    pub lifetime: Duration,
}

impl Default for DelegationRequest {
    fn default() -> Self {
        Self {
            verbs: AGENT_VERBS,
            lifetime: DEFAULT_LIFETIME,
        }
    }
}

/// Why a delegation was refused. Nothing was minted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DelegationError {
    /// The launcher has no live grant (before HELLO, revoked, or expired).
    #[error("the launching connection holds no live grant")]
    NoLauncherGrant,
    /// The session does not exist.
    #[error("no such session")]
    NoSession,
    /// The request names no verb.
    #[error("a delegated credential needs at least one verb")]
    NoVerbs,
    /// The request names a verb no agent credential may carry.
    #[error("{} cannot be delegated to an agent", .0.name())]
    Widens(Verb),
    /// The launcher does not itself hold this verb on the whole session.
    #[error("the launcher does not hold {} on that session", .0.name())]
    LauncherLacks(Verb),
    /// A zero lifetime, or a launcher whose own grant has already lapsed.
    #[error("a delegated credential needs a lifetime later than now")]
    Lifetime,
    /// The random id could not be drawn.
    #[error("could not mint a credential id: {0}")]
    Random(String),
    /// The credential's socket could not be opened.
    #[error("could not open the credential's socket: {0}")]
    Socket(String),
}

/// One delegated credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegatedCredential {
    /// The credential id, `delegated:` then 32 hex digits.
    pub id: String,
    /// The session it is scoped to.
    pub session: SessionId,
    /// That session's wire id: the `group:` selector of its grant.
    pub group: u32,
    /// The delegated verbs.
    pub verbs: Verbs,
    /// The human whose grant it exercises.
    pub on_behalf_of: PrincipalId,
    /// The principal that minted it (the human, or a delegate narrowing its
    /// own grant).
    pub delegator: PrincipalId,
    /// When it stops admitting anything.
    pub expires_at: DateTime<Utc>,
}

impl DelegatedCredential {
    /// The principal its connections act as.
    #[must_use]
    pub fn principal(&self) -> PrincipalId {
        PrincipalId::Credential(self.id.clone())
    }

    /// Its scope: the delegated verbs on its session's Group.
    ///
    /// # Panics
    ///
    /// Never: one nonempty grant is always a canonical set.
    #[must_use]
    pub fn scopes(&self) -> TerminalScopeSet {
        TerminalScopeSet::from_grants([ScopeGrant::new(self.verbs, Selector::Group(self.group))])
            .unwrap_or_default()
    }
}

/// The live delegated credentials, by id. Withdrawn and lapsed ones are
/// dropped, so a lookup that misses refuses.
#[derive(Debug, Default)]
pub struct DelegationTable {
    live: HashMap<String, DelegatedCredential>,
    /// Each credential's own socket, once opened: its path and the
    /// cancellation that closes it.
    listeners: HashMap<String, (PathBuf, tokio_util::sync::CancellationToken)>,
    /// The credential each `DELEGATE_AGENT` operation minted, so a retry
    /// answers it instead of minting another.
    operations: HashMap<[u8; 16], String>,
}

impl DelegationTable {
    /// Record a freshly minted credential.
    pub(crate) fn insert(&mut self, credential: DelegatedCredential) {
        self.live.insert(credential.id.clone(), credential);
    }

    /// The credential `id` names, while it is live at `now`.
    #[must_use]
    pub fn live(&self, id: &str, now: DateTime<Utc>) -> Option<&DelegatedCredential> {
        self.live
            .get(id)
            .filter(|credential| credential.expires_at > now)
    }

    /// Record `id`'s socket at `path` and the cancellation that closes it.
    /// `false` when `id` was withdrawn meanwhile, so the caller closes it at
    /// once.
    pub(crate) fn attach_listener(
        &mut self,
        id: &str,
        path: PathBuf,
        close: tokio_util::sync::CancellationToken,
    ) -> bool {
        if !self.live.contains_key(id) {
            return false;
        }
        self.listeners.insert(id.to_owned(), (path, close));
        true
    }

    /// The socket `id`'s connections arrive on, once opened.
    #[must_use]
    pub fn socket_path(&self, id: &str) -> Option<&Path> {
        self.listeners.get(id).map(|(path, _)| path.as_path())
    }

    /// Remember that `operation` minted `id`.
    pub(crate) fn note_operation(&mut self, operation: [u8; 16], id: &str) {
        self.operations.insert(operation, id.to_owned());
    }

    /// The credential `operation` minted, if it is still on file.
    #[must_use]
    pub fn minted_by(&self, operation: &[u8; 16]) -> Option<&str> {
        self.operations.get(operation).map(String::as_str)
    }

    /// Withdraw `id`, closing its socket; the credential it named, if it
    /// was live.
    pub(crate) fn remove(&mut self, id: &str) -> Option<DelegatedCredential> {
        if let Some((_, close)) = self.listeners.remove(id) {
            close.cancel();
        }
        self.operations.retain(|_, minted| minted != id);
        self.live.remove(id)
    }

    /// Every credential scoped to `session`.
    #[must_use]
    pub fn for_session(&self, session: SessionId) -> Vec<String> {
        self.live
            .values()
            .filter(|credential| credential.session == session)
            .map(|credential| credential.id.clone())
            .collect()
    }

    /// The number of live credentials.
    #[must_use]
    pub fn len(&self) -> usize {
        self.live.len()
    }

    /// Whether no credential is live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }
}

/// A fresh credential id: [`ID_PREFIX`] then 128 random bits in hex.
///
/// # Errors
///
/// [`DelegationError::Random`] when the system RNG fails.
pub fn mint_id() -> Result<String, DelegationError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| DelegationError::Random(error.to_string()))?;
    Ok(format!("{ID_PREFIX}{}", hex::encode(bytes)))
}

/// Whether `id` is spelled as a delegated credential id.
#[must_use]
pub fn is_delegated_id(id: &str) -> bool {
    id.strip_prefix(ID_PREFIX)
        .is_some_and(|rest| rest.len() == 32 && rest.bytes().all(|b| b.is_ascii_hexdigit()))
}
