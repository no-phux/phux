//! Bearer-credential authentication for remote consumers (ADR-0092). The
//! store holds only SHA-256 verifiers, never secrets.

use std::io;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

#[cfg(test)]
use chrono::Duration;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

mod credential;
mod lifecycle;
mod persist;
mod reloading;
mod store;

pub use self::credential::*;
pub use self::lifecycle::*;
#[cfg(test)]
use self::persist::credential_lock_path;
use self::persist::{AtomicWriteFault, atomic_write, atomic_write_with_fault, with_store_lock};
pub use self::reloading::*;
pub use self::store::*;

#[cfg(test)]
mod tests;

/// The bearer secret an on-demand listener admits (`OPEN_LISTENER`,
/// ADR-0120): in memory only, as a SHA-256 verifier compared in constant
/// time, and revoked by closing the listener.
pub struct ListenerToken {
    verifier: [u8; 32],
    id: String,
    issued_at: DateTime<Utc>,
}

impl std::fmt::Debug for ListenerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListenerToken")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl ListenerToken {
    /// The principal a listener token authenticates as, which is what
    /// `phux whoami` reports for such a connection.
    pub const PRINCIPAL: &'static str = "ssh-bootstrap";

    /// Mint a token and its one-time secret.
    ///
    /// # Errors
    ///
    /// When the OS CSPRNG cannot be read.
    pub fn mint() -> Result<(Self, [u8; TOKEN_LEN]), AuthError> {
        let (id, secret) = random_identity_and_secret()?;
        let mut verifier = [0u8; 32];
        verifier.copy_from_slice(&Sha256::digest(secret));
        let token = Self {
            verifier,
            id: format!("listener:{id}"),
            issued_at: Utc::now(),
        };
        Ok((token, secret))
    }

    /// The credential id, stable for the listener's life.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Authenticate a presented secret. Grants what a pairing-store
    /// credential grants, so a bootstrapped attach can drive its panes.
    #[must_use]
    pub fn authenticate(&self, presented: &[u8]) -> Option<AuthenticatedCredential> {
        if presented.len() != TOKEN_LEN {
            return None;
        }
        let candidate = Sha256::digest(presented);
        bool::from(self.verifier.ct_eq(candidate.as_slice())).then(|| AuthenticatedCredential {
            id: self.id.clone(),
            principal: Self::PRINCIPAL.to_owned(),
            scopes: vec![TERMINAL_CONTROL_SCOPE.to_owned()],
            issued_at: self.issued_at,
            expires_at: None,
            generation: 1,
            registry_instance: None,
        })
    }
}

/// Default persisted path for the remote-consumer credential store.
#[must_use]
pub fn default_token_store_path() -> PathBuf {
    crate::telemetry::state_dir().join("remote-tokens")
}

/// Length in bytes of a bearer secret minted from the OS CSPRNG.
pub const TOKEN_LEN: usize = 32;
const STORE_VERSION: u32 = 1;
const VERIFIER_PREFIX: &str = "sha256:";
// One read plus two retries bounds each authentication attempt; a later
// authentication retries again if writers keep replacing the store.
const STABLE_READ_ATTEMPTS: usize = 3;

/// The initial scope of a terminal pairing; work-plane access is not
/// implied (ADR-0092).
pub const TERMINAL_CONTROL_SCOPE: &str = "terminal.control";

/// Errors from loading or changing credentials.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The credential file could not be read or written.
    #[error("credential store io: {0}")]
    Io(#[from] io::Error),
    /// The OS random source failed while minting a credential.
    #[error("os random source unavailable: {0}")]
    Random(#[from] getrandom::Error),
    /// The structured store could not be decoded or violates its invariants.
    #[error("malformed credential store: {0}")]
    Malformed(String),
    /// The file holds anonymous token lines, the store format before
    /// versioning. Their migration is retired (ADR-0179): such a store
    /// admits nobody until it is removed and its devices pair again.
    #[error(
        "unsupported legacy token store (anonymous token lines, retired by ADR-0179): remove it and pair devices again with `phux pair`"
    )]
    LegacyStore,
    /// A requested credential does not exist.
    #[error("credential {0} not found")]
    CredentialNotFound(String),
    /// Rotation cannot reactivate a credential that has already been revoked.
    #[error("credential {0} is revoked")]
    CredentialRevoked(String),
    /// Rotation cannot issue another generation after absolute expiry.
    #[error("credential {0} is expired")]
    CredentialExpired(String),
    /// The store changed during every bounded stable-read attempt.
    #[error("credential store changed during {STABLE_READ_ATTEMPTS} consecutive reads")]
    UnstableStore,
    /// Credential stores must be regular, owner-only files owned by this user.
    #[error("insecure credential store: {0}")]
    InsecureStore(String),
    /// A development build aimed at the production credential store
    /// ([`phux_config::production::refuse_dev_on_production_state`]).
    #[error("{0}")]
    ProductionStore(String),
}

#[cfg(test)]
pub(crate) fn write_test_credential(path: &Path, secret: &[u8; TOKEN_LEN]) {
    let file = CredentialFile {
        version: STORE_VERSION,
        credentials: vec![CredentialRecord {
            id: "test-credential".to_owned(),
            verifier: verifier(secret),
            principal: "test-principal".to_owned(),
            scopes: vec![TERMINAL_CONTROL_SCOPE.to_owned()],
            issued_at: Utc::now() - Duration::seconds(1),
            expires_at: None,
            revoked_at: None,
            generation: 1,
            last_seen: None,
        }],
    };
    atomic_write(path, &file).unwrap();
}
