//! Structured bearer-credential authentication for remote consumers.
//!
//! The on-disk store contains only SHA-256 verifiers, never bearer secrets.
//! Credentials carry stable identity and authorization metadata for the
//! authority boundary described by ADR-0092; scope enforcement is deliberately
//! owned by the follow-up authorization work, not this module.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use phux_protocol::policy::PeerIdentity;

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

/// The initial scope of an ordinary terminal pairing. Work-plane access is
/// intentionally absent: ADR-0092 says existing terminal pairing is not
/// implicitly work authorization.
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
    /// Anonymous token lines require an explicit one-time conversion.
    #[error("legacy token store requires explicit migration")]
    LegacyMigrationRequired,
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
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFile {
    version: u32,
    credentials: Vec<CredentialRecord>,
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
struct CredentialRecord {
    id: String,
    verifier: String,
    principal: String,
    scopes: Vec<String>,
    issued_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    generation: u64,
}

/// Identity and policy metadata captured when a connection is established.
///
/// This value is a snapshot. Revocation and expiry apply to the next
/// authentication attempt; an established session is not re-authorized and
/// keeps this attestation until its transport closes (ADR-0031).
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
    /// Monotonic generation within the credential identifier.
    pub generation: u64,
}

/// Transport-derived identity plus the credential attestation captured at
/// connection establishment.
///
/// The credential is absent for local/SSH trust paths. It is retained unchanged
/// for the connection lifetime so the downstream authorization seam can inspect
/// principal, scopes, generation, expiry, and id without re-reading the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionIdentity {
    /// Existing transport identity used by current terminal policy.
    pub peer: PeerIdentity,
    /// Structured remote credential, when bearer authentication was used.
    pub credential: Option<AuthenticatedCredential>,
}

impl From<PeerIdentity> for ConnectionIdentity {
    fn from(peer: PeerIdentity) -> Self {
        Self {
            peer,
            credential: None,
        }
    }
}

impl std::ops::Deref for ConnectionIdentity {
    type Target = PeerIdentity;

    fn deref(&self) -> &Self::Target {
        &self.peer
    }
}

/// A newly minted bearer secret and its non-secret identity.
pub struct MintedCredential {
    /// Stable identifier of the credential.
    pub id: String,
    /// Newly minted generation number.
    pub generation: u64,
    secret: String,
    durable: bool,
}

impl MintedCredential {
    /// The bearer secret, exposed only for one-time delivery to the consumer.
    #[must_use]
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// Whether the containing directory was successfully synced after rename.
    /// `false` means the credential is active and visible, but a crash could
    /// lose the directory entry; callers must not retry and mint another secret.
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

/// A parsed snapshot of the current structured credential store.
#[derive(Clone, Default)]
pub struct TokenStore {
    file: CredentialFile,
}

impl std::fmt::Debug for TokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenStore")
            .field("credentials", &self.file.credentials.len())
            .finish()
    }
}

impl TokenStore {
    /// Load a versioned store. Missing files are empty; legacy token lines are
    /// rejected until [`migrate_legacy_store`] is called explicitly.
    pub fn load(path: &Path) -> Result<Self, AuthError> {
        let raw = read_secure_store(path)?.unwrap_or_default();
        if raw.trim().is_empty() {
            return Ok(Self {
                file: CredentialFile::default(),
            });
        }
        if !raw.trim_start().starts_with('{') {
            return Err(AuthError::LegacyMigrationRequired);
        }
        let file: CredentialFile =
            serde_json::from_str(&raw).map_err(|error| AuthError::Malformed(error.to_string()))?;
        validate_file(&file)?;
        Ok(Self { file })
    }

    /// Number of credential generations in this snapshot.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.file.credentials.len()
    }

    /// Whether this snapshot has no credentials.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.file.credentials.is_empty()
    }

    /// Authenticate a bearer secret at the current wall-clock time.
    #[must_use]
    pub fn authenticate(&self, presented: &[u8]) -> Option<AuthenticatedCredential> {
        self.authenticate_at(presented, Utc::now())
    }

    fn authenticate_at(
        &self,
        presented: &[u8],
        now: DateTime<Utc>,
    ) -> Option<AuthenticatedCredential> {
        if presented.len() != TOKEN_LEN {
            return None;
        }
        let candidate = Sha256::digest(presented);
        let mut matched = None;
        for record in &self.file.credentials {
            let verifier = decode_verifier(&record.verifier).ok();
            let active = record.revoked_at.is_none()
                && record.expires_at.is_none_or(|expiry| now < expiry)
                && record.issued_at <= now;
            let is_match = verifier
                .as_ref()
                .is_some_and(|verifier| bool::from(verifier.ct_eq(candidate.as_slice())));
            if active && is_match {
                matched = Some(AuthenticatedCredential {
                    id: record.id.clone(),
                    principal: record.principal.clone(),
                    scopes: record.scopes.clone(),
                    issued_at: record.issued_at,
                    expires_at: record.expires_at,
                    generation: record.generation,
                });
            }
        }
        matched
    }

    /// Compatibility predicate for transport callers that need only admission.
    #[must_use]
    pub fn verify(&self, presented: &[u8]) -> bool {
        self.authenticate(presented).is_some()
    }
}

fn validate_store_metadata(path: &Path, metadata: &fs::Metadata) -> Result<(), AuthError> {
    validate_store_metadata_for_uid(path, metadata, rustix::process::geteuid().as_raw())
}

fn validate_store_metadata_for_uid(
    path: &Path,
    metadata: &fs::Metadata,
    expected_uid: u32,
) -> Result<(), AuthError> {
    if !metadata.file_type().is_file() {
        return Err(AuthError::InsecureStore(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if metadata.uid() != expected_uid {
        return Err(AuthError::InsecureStore(format!(
            "{} is owned by uid {}, expected effective uid {expected_uid}",
            path.display(),
            metadata.uid()
        )));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(AuthError::InsecureStore(format!(
            "{} has mode {:04o}; group and world permissions must be zero",
            path.display(),
            metadata.mode() & 0o777
        )));
    }
    if metadata.mode() & 0o400 == 0 {
        return Err(AuthError::InsecureStore(format!(
            "{} has mode {:04o}; owner read permission is required",
            path.display(),
            metadata.mode() & 0o777
        )));
    }
    Ok(())
}

fn read_secure_store(path: &Path) -> Result<Option<String>, AuthError> {
    let path_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if path_metadata.file_type().is_symlink() {
        return Err(AuthError::InsecureStore(format!(
            "{} is a symbolic link",
            path.display()
        )));
    }
    validate_store_metadata(path, &path_metadata)?;

    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    validate_store_metadata(path, &input.metadata()?)?;
    let mut raw = String::new();
    input.read_to_string(&mut raw)?;
    Ok(Some(raw))
}

fn validate_file(file: &CredentialFile) -> Result<(), AuthError> {
    if file.version != STORE_VERSION {
        return Err(AuthError::Malformed(format!(
            "unsupported version {} (expected {STORE_VERSION})",
            file.version
        )));
    }
    let mut keys = std::collections::HashSet::new();
    for record in &file.credentials {
        if record.id.is_empty() || record.principal.is_empty() || record.generation == 0 {
            return Err(AuthError::Malformed(
                "credential id, principal, and generation must be present".to_owned(),
            ));
        }
        decode_verifier(&record.verifier)?;
        if !keys.insert((&record.id, record.generation)) {
            return Err(AuthError::Malformed(format!(
                "duplicate credential generation {}:{}",
                record.id, record.generation
            )));
        }
    }
    Ok(())
}

fn verifier(secret: &[u8]) -> String {
    format!("{VERIFIER_PREFIX}{}", hex::encode(Sha256::digest(secret)))
}

fn decode_verifier(encoded: &str) -> Result<[u8; 32], AuthError> {
    let hex = encoded
        .strip_prefix(VERIFIER_PREFIX)
        .ok_or_else(|| AuthError::Malformed("credential verifier must use sha256".to_owned()))?;
    let bytes = hex::decode(hex)
        .map_err(|_| AuthError::Malformed("credential verifier is not hex".to_owned()))?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| AuthError::Malformed("credential verifier has wrong length".to_owned()))
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
struct Stamp {
    mtime: Option<std::time::SystemTime>,
    len: u64,
    dev: u64,
    ino: u64,
    uid: u32,
    mode: u32,
    ctime: i64,
    ctime_nsec: i64,
}

impl Stamp {
    fn probe(path: &Path) -> Result<Option<Self>, AuthError> {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if meta.file_type().is_symlink() {
            return Err(AuthError::InsecureStore(format!(
                "{} is a symbolic link",
                path.display()
            )));
        }
        validate_store_metadata(path, &meta)?;
        Ok(Some(Self {
            mtime: meta.modified().ok(),
            len: meta.len(),
            dev: meta.dev(),
            ino: meta.ino(),
            uid: meta.uid(),
            mode: meta.mode(),
            ctime: meta.ctime(),
            ctime_nsec: meta.ctime_nsec(),
        }))
    }
}

struct Cached {
    stamp: Option<Stamp>,
    store: TokenStore,
    reloads: u64,
}

/// A last-known-good snapshot that re-reads after each atomic file generation.
pub struct ReloadingTokenStore {
    path: PathBuf,
    cached: std::sync::Mutex<Cached>,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the cached field contains credential verifiers and is deliberately redacted"
)]
impl std::fmt::Debug for ReloadingTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReloadingTokenStore")
            .field("path", &self.path)
            .field("credentials", &self.len())
            .finish()
    }
}

impl ReloadingTokenStore {
    const fn from_snapshot(path: PathBuf, stamp: Option<Stamp>, store: TokenStore) -> Self {
        Self {
            path,
            cached: std::sync::Mutex::new(Cached {
                stamp,
                store,
                reloads: 0,
            }),
        }
    }

    /// Load the current snapshot and begin tracking its file generation.
    pub fn load(path: PathBuf) -> Result<Self, AuthError> {
        Self::load_observed(path, |_| {})
    }

    fn load_observed(path: PathBuf, after_load: impl FnMut(usize)) -> Result<Self, AuthError> {
        let (stamp, store) = stable_load_observed(&path, after_load)?;
        Ok(Self::from_snapshot(path, stamp, store))
    }

    /// Path of the tracked credential store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn with_current<T>(&self, f: impl FnOnce(&TokenStore) -> T) -> T {
        self.with_current_observed(|_| {}, f)
    }

    fn with_current_observed<T>(
        &self,
        after_load: impl FnMut(usize),
        f: impl FnOnce(&TokenStore) -> T,
    ) -> T {
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stamp = match Stamp::probe(&self.path) {
            Ok(stamp) => stamp,
            Err(error) => {
                tracing::warn!(path = %self.path.display(), %error, "credential store integrity check failed; denying authentication");
                cached.stamp = None;
                cached.store = TokenStore::default();
                return f(&cached.store);
            }
        };
        if stamp != cached.stamp {
            match stable_load_observed(&self.path, after_load) {
                Ok((stamp, store)) => {
                    cached.stamp = stamp;
                    cached.store = store;
                    cached.reloads = cached.reloads.saturating_add(1);
                }
                Err(error) => {
                    tracing::warn!(
                        path = %self.path.display(), %error,
                        "changed credential store could not be loaded; denying authentication"
                    );
                    cached.stamp = None;
                    cached.store = TokenStore::default();
                    return f(&cached.store);
                }
            }
        }
        f(&cached.store)
    }

    /// Authenticate against the current readable generation.
    #[must_use]
    pub fn authenticate(&self, presented: &[u8]) -> Option<AuthenticatedCredential> {
        self.with_current(|store| store.authenticate(presented))
    }

    /// Whether a bearer secret authenticates against the current generation.
    #[must_use]
    pub fn verify(&self, presented: &[u8]) -> bool {
        self.authenticate(presented).is_some()
    }

    /// Number of credential generations in the current snapshot.
    #[must_use]
    pub fn len(&self) -> usize {
        self.with_current(TokenStore::len)
    }

    /// Whether the current snapshot contains no credentials.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.with_current(TokenStore::is_empty)
    }

    #[cfg(test)]
    fn reloads(&self) -> u64 {
        self.cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reloads
    }

    #[cfg(test)]
    fn cached_is_empty(&self) -> bool {
        self.cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .store
            .is_empty()
    }
}

fn stable_load_observed(
    path: &Path,
    mut after_load: impl FnMut(usize),
) -> Result<(Option<Stamp>, TokenStore), AuthError> {
    for attempt in 0..STABLE_READ_ATTEMPTS {
        let before = Stamp::probe(path)?;
        let loaded = TokenStore::load(path);
        after_load(attempt);
        let after = Stamp::probe(path)?;
        if before == after {
            return loaded.map(|store| (after, store));
        }
    }
    Err(AuthError::UnstableStore)
}

/// Mint a generation-one credential with terminal-only authority.
pub fn mint_token(path: &Path) -> Result<MintedCredential, AuthError> {
    mint_credential(path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None)
}

/// Mint a structured credential. `principal = None` creates a stable principal
/// from the generated credential id.
pub fn mint_credential(
    path: &Path,
    principal: Option<&str>,
    scopes: &[String],
    expires_at: Option<DateTime<Utc>>,
) -> Result<MintedCredential, AuthError> {
    with_store_lock(path, || {
        mint_credential_unlocked(path, principal, scopes, expires_at)
    })
}

fn mint_credential_unlocked(
    path: &Path,
    principal: Option<&str>,
    scopes: &[String],
    expires_at: Option<DateTime<Utc>>,
) -> Result<MintedCredential, AuthError> {
    let mut file = load_file_for_update(path)?;
    let (id, secret) = random_identity_and_secret()?;
    let principal = principal.map_or_else(|| format!("remote-consumer:{id}"), str::to_owned);
    file.credentials.push(CredentialRecord {
        id: id.clone(),
        verifier: verifier(&secret),
        principal,
        scopes: scopes.to_vec(),
        issued_at: Utc::now(),
        expires_at,
        revoked_at: None,
        generation: 1,
    });
    let commit = atomic_write(path, &file)?;
    Ok(MintedCredential {
        id,
        generation: 1,
        secret: hex::encode(secret),
        durable: commit.is_durable(),
    })
}

/// Rotate a credential with a bounded overlap.
///
/// The atomic replacement means interruption exposes either the old complete
/// store or the new complete store, never half a rotation.
pub fn rotate_credential(
    path: &Path,
    id: &str,
    overlap: Duration,
) -> Result<MintedCredential, AuthError> {
    with_store_lock(path, || {
        rotate_credential_at(path, id, overlap, Utc::now(), AtomicWriteFault::None)
    })
}

fn rotate_credential_at(
    path: &Path,
    id: &str,
    overlap: Duration,
    now: DateTime<Utc>,
    fault: AtomicWriteFault,
) -> Result<MintedCredential, AuthError> {
    let mut file = load_file_for_update(path)?;
    let latest = file
        .credentials
        .iter()
        .filter(|record| record.id == id)
        .max_by_key(|record| record.generation)
        .cloned()
        .ok_or_else(|| AuthError::CredentialNotFound(id.to_owned()))?;
    if latest.revoked_at.is_some() {
        return Err(AuthError::CredentialRevoked(id.to_owned()));
    }
    if latest.expires_at.is_some_and(|expiry| now >= expiry) {
        return Err(AuthError::CredentialExpired(id.to_owned()));
    }
    let overlap_until = now + overlap.max(Duration::zero());
    for record in file.credentials.iter_mut().filter(|record| record.id == id) {
        if record.revoked_at.is_none() {
            record.expires_at = Some(
                record
                    .expires_at
                    .map_or(overlap_until, |expiry| expiry.min(overlap_until)),
            );
        }
    }
    let mut secret = [0u8; TOKEN_LEN];
    getrandom::getrandom(&mut secret)?;
    let generation = latest.generation.saturating_add(1);
    file.credentials.push(CredentialRecord {
        id: id.to_owned(),
        verifier: verifier(&secret),
        principal: latest.principal,
        scopes: latest.scopes,
        issued_at: now,
        expires_at: latest.expires_at,
        revoked_at: None,
        generation,
    });
    let commit = atomic_write_with_fault(path, &file, fault)?;
    Ok(MintedCredential {
        id: id.to_owned(),
        generation,
        secret: hex::encode(secret),
        durable: commit.is_durable(),
    })
}

/// Revoke every generation of a credential for future connection attempts.
pub fn revoke_credential(path: &Path, id: &str) -> Result<CommitOutcome, AuthError> {
    with_store_lock(path, || {
        let mut file = load_file_for_update(path)?;
        let now = Utc::now();
        let mut found = false;
        for record in file.credentials.iter_mut().filter(|record| record.id == id) {
            record.revoked_at = Some(now);
            found = true;
        }
        if !found {
            return Err(AuthError::CredentialNotFound(id.to_owned()));
        }
        atomic_write(path, &file)
    })
}

/// Explicitly convert anonymous token lines to generation-one structured
/// records. The old bearer values are read once and replaced by verifiers.
pub fn migrate_legacy_store(path: &Path) -> Result<MigrationOutcome, AuthError> {
    with_store_lock(path, || {
        let raw = read_secure_store(path)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "credential store does not exist")
        })?;
        if raw.trim_start().starts_with('{') {
            TokenStore::load(path)?;
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            let durable = FileSync::sync_directory(parent).is_ok();
            return Ok(MigrationOutcome {
                migrated: 0,
                durable,
            });
        }
        let now = Utc::now();
        let mut file = CredentialFile::default();
        let mut migrated_ids = std::collections::HashSet::new();
        for line in raw.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let secret = decode_secret(line)?;
            let id = legacy_peer_id(&secret);
            if !migrated_ids.insert(id.clone()) {
                continue;
            }
            file.credentials.push(CredentialRecord {
                id: id.clone(),
                verifier: verifier(&secret),
                principal: format!("legacy-remote-consumer:{id}"),
                scopes: vec![TERMINAL_CONTROL_SCOPE.to_owned()],
                issued_at: now,
                expires_at: None,
                revoked_at: None,
                generation: 1,
            });
        }
        let count = file.credentials.len();
        let commit = atomic_write(path, &file)?;
        Ok(MigrationOutcome {
            migrated: count,
            durable: commit.is_durable(),
        })
    })
}

fn load_file_for_update(path: &Path) -> Result<CredentialFile, AuthError> {
    Ok(TokenStore::load(path)?.file)
}

fn random_identity_and_secret() -> Result<(String, [u8; TOKEN_LEN]), AuthError> {
    let mut id = [0u8; 16];
    let mut secret = [0u8; TOKEN_LEN];
    getrandom::getrandom(&mut id)?;
    getrandom::getrandom(&mut secret)?;
    Ok((hex::encode(id), secret))
}

fn legacy_peer_id(secret: &[u8]) -> String {
    let digest = Sha256::digest(secret);
    hex::encode(&digest[..8])
}

fn decode_secret(encoded: &str) -> Result<[u8; TOKEN_LEN], AuthError> {
    let bytes = hex::decode(encoded)
        .map_err(|_| AuthError::Malformed("legacy token is not hex".to_owned()))?;
    <[u8; TOKEN_LEN]>::try_from(bytes.as_slice())
        .map_err(|_| AuthError::Malformed("legacy token has wrong length".to_owned()))
}

/// Outcome of an atomic store replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitOutcome {
    durable: bool,
}

/// Result of an idempotent legacy conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MigrationOutcome {
    migrated: usize,
    durable: bool,
}

impl MigrationOutcome {
    /// Number of anonymous token lines converted by this call.
    #[must_use]
    pub const fn migrated(self) -> usize {
        self.migrated
    }

    /// Whether the converted directory entry reached stable storage.
    #[must_use]
    pub const fn is_durable(self) -> bool {
        self.durable
    }
}

impl CommitOutcome {
    /// Whether both file contents and the containing directory entry reached
    /// stable storage. A false result is already visible and must not be retried.
    #[must_use]
    pub const fn is_durable(self) -> bool {
        self.durable
    }
}

struct StoreLock {
    file: fs::File,
    parent: fs::File,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.file, rustix::fs::FlockOperation::Unlock);
        let _ = rustix::fs::flock(&self.parent, rustix::fs::FlockOperation::Unlock);
    }
}

fn credential_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn credential_lock_path(path: &Path) -> PathBuf {
    let parent = credential_parent(path);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("credentials");
    parent.join(format!(".{name}.lock"))
}

fn validate_lock_parent(path: &Path, metadata: &fs::Metadata) -> Result<(), AuthError> {
    if !metadata.file_type().is_dir() {
        return Err(AuthError::InsecureStore(format!(
            "credential parent {} is not a directory",
            path.display()
        )));
    }
    let expected_uid = rustix::process::geteuid().as_raw();
    if metadata.uid() != expected_uid {
        return Err(AuthError::InsecureStore(format!(
            "credential parent {} is owned by uid {}, expected effective uid {expected_uid}",
            path.display(),
            metadata.uid()
        )));
    }
    let mode = metadata.mode() & 0o777;
    if mode & 0o022 != 0 || mode & 0o700 != 0o700 {
        return Err(AuthError::InsecureStore(format!(
            "credential parent {} has mode {mode:04o}; owner rwx and no group/world write permission are required",
            path.display()
        )));
    }
    Ok(())
}

fn validate_lock_file(path: &Path, metadata: &fs::Metadata) -> Result<(), AuthError> {
    if !metadata.file_type().is_file() {
        return Err(AuthError::InsecureStore(format!(
            "credential lock {} is not a regular file",
            path.display()
        )));
    }
    let expected_uid = rustix::process::geteuid().as_raw();
    if metadata.uid() != expected_uid {
        return Err(AuthError::InsecureStore(format!(
            "credential lock {} is owned by uid {}, expected effective uid {expected_uid}",
            path.display(),
            metadata.uid()
        )));
    }
    let mode = metadata.mode() & 0o777;
    if mode != 0o600 {
        return Err(AuthError::InsecureStore(format!(
            "credential lock {} has mode {mode:04o}; expected 0600",
            path.display()
        )));
    }
    Ok(())
}

fn validate_open_path_identity(
    path: &Path,
    open_metadata: &fs::Metadata,
    validate: impl FnOnce(&Path, &fs::Metadata) -> Result<(), AuthError>,
) -> Result<(), AuthError> {
    let path_metadata = fs::symlink_metadata(path)?;
    validate(path, &path_metadata)?;
    if path_metadata.dev() != open_metadata.dev() || path_metadata.ino() != open_metadata.ino() {
        return Err(AuthError::InsecureStore(format!(
            "{} was replaced during lock acquisition",
            path.display()
        )));
    }
    Ok(())
}

fn open_lock_parent(parent: &Path) -> Result<fs::File, AuthError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(parent)?;
    let path_metadata = fs::symlink_metadata(parent)?;
    if path_metadata.file_type().is_symlink() {
        return Err(AuthError::InsecureStore(format!(
            "credential parent {} is a symbolic link",
            parent.display()
        )));
    }
    validate_lock_parent(parent, &path_metadata)?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(parent)?;
    let open_metadata = directory.metadata()?;
    validate_lock_parent(parent, &open_metadata)?;
    validate_open_path_identity(parent, &open_metadata, validate_lock_parent)?;
    Ok(directory)
}

fn with_store_lock<T>(
    path: &Path,
    operation: impl FnOnce() -> Result<T, AuthError>,
) -> Result<T, AuthError> {
    let parent = credential_parent(path);
    let parent_lock = open_lock_parent(parent)?;
    rustix::fs::flock(&parent_lock, rustix::fs::FlockOperation::LockExclusive)
        .map_err(io::Error::from)?;
    let parent_metadata = parent_lock.metadata()?;
    validate_open_path_identity(parent, &parent_metadata, validate_lock_parent)?;

    let lock_path = credential_lock_path(path);
    match fs::symlink_metadata(&lock_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(AuthError::InsecureStore(format!(
                    "credential lock {} is a symbolic link",
                    lock_path.display()
                )));
            }
            validate_lock_file(&lock_path, &metadata)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&lock_path)?;
    validate_lock_file(&lock_path, &lock.metadata()?)?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).map_err(io::Error::from)?;
    let lock_metadata = lock.metadata()?;
    validate_open_path_identity(&lock_path, &lock_metadata, validate_lock_file)?;
    let _guard = StoreLock {
        file: lock,
        parent: parent_lock,
    };
    operation()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AtomicWriteFault {
    None,
    BeforeTempSync,
    AfterTempSyncBeforeRename,
    AfterRenameBeforeDirectorySync,
}

fn injected_failure(stage: &str) -> AuthError {
    io::Error::other(format!("injected atomic-write interruption at {stage}")).into()
}

fn atomic_write(path: &Path, file: &CredentialFile) -> Result<CommitOutcome, AuthError> {
    atomic_write_with_fault(path, file, AtomicWriteFault::None)
}

fn atomic_write_with_fault(
    path: &Path,
    file: &CredentialFile,
    fault: AtomicWriteFault,
) -> Result<CommitOutcome, AuthError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut suffix = [0u8; 8];
    getrandom::getrandom(&mut suffix)?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("credentials"),
        hex::encode(suffix)
    ));
    let result = (|| {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        serde_json::to_writer_pretty(&mut output, file)
            .map_err(|error| AuthError::Malformed(error.to_string()))?;
        output.write_all(b"\n")?;
        if fault == AtomicWriteFault::BeforeTempSync {
            return Err(injected_failure("before temp-file fsync"));
        }
        output.sync_all()?;
        if fault == AtomicWriteFault::AfterTempSyncBeforeRename {
            return Err(injected_failure("after temp-file fsync"));
        }
        fs::rename(&tmp, path)?;
        if fault == AtomicWriteFault::AfterRenameBeforeDirectorySync {
            return Ok(CommitOutcome { durable: false });
        }
        match FileSync::sync_directory(parent) {
            Ok(()) => Ok(CommitOutcome { durable: true }),
            Err(error) => {
                // Rename already committed. Reporting an ordinary error would
                // invite a retry that mints another secret or resurrects stale
                // state. Return a visible-but-not-durable outcome instead.
                tracing::warn!(path = %path.display(), %error, "credential store replacement is visible but directory fsync failed; do not retry the mutation");
                Ok(CommitOutcome { durable: false })
            }
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

struct FileSync;

impl FileSync {
    fn sync_directory(path: &Path) -> io::Result<()> {
        fs::File::open(path)?.sync_all()
    }
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
        }],
    };
    atomic_write(path, &file).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn secret(minted: &MintedCredential) -> Vec<u8> {
        hex::decode(minted.secret()).unwrap()
    }

    #[test]
    fn structured_mint_persists_only_a_redacted_verifier() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let minted = mint_credential(
            &path,
            Some("device:cockpit"),
            &[TERMINAL_CONTROL_SCOPE.to_owned()],
            None,
        )
        .unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"version\": 1"));
        assert!(raw.contains("device:cockpit"));
        assert!(raw.contains("sha256:"));
        assert!(!raw.contains(minted.secret()));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let auth = TokenStore::load(&path)
            .unwrap()
            .authenticate(&secret(&minted))
            .unwrap();
        assert_eq!(auth.id, minted.id);
        assert_eq!(auth.principal, "device:cockpit");
        assert_eq!(auth.scopes, [TERMINAL_CONTROL_SCOPE]);
        assert_eq!(auth.generation, 1);
    }

    #[test]
    fn legacy_store_requires_and_survives_explicit_migration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let token = "ab".repeat(TOKEN_LEN);
        fs::write(&path, format!("# old\n{token}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            TokenStore::load(&path),
            Err(AuthError::LegacyMigrationRequired)
        ));
        assert_eq!(migrate_legacy_store(&path).unwrap().migrated(), 1);
        assert_eq!(
            migrate_legacy_store(&path).unwrap().migrated(),
            0,
            "migration is idempotent"
        );
        let raw = fs::read_to_string(&path).unwrap();
        assert!(!raw.contains(&token));
        let store = TokenStore::load(&path).unwrap();
        let secret = hex::decode(token).unwrap();
        let authenticated = store.authenticate(&secret).unwrap();
        assert_eq!(authenticated.id, legacy_peer_id(&secret));
    }

    #[test]
    fn expiry_and_revocation_fail_closed() {
        let now = Utc::now();
        let token = [0x44; TOKEN_LEN];
        let base = CredentialRecord {
            id: "cred".to_owned(),
            verifier: verifier(&token),
            principal: "device:test".to_owned(),
            scopes: vec![TERMINAL_CONTROL_SCOPE.to_owned()],
            issued_at: now - Duration::minutes(1),
            expires_at: Some(now + Duration::seconds(1)),
            revoked_at: None,
            generation: 1,
        };
        let store = TokenStore {
            file: CredentialFile {
                version: 1,
                credentials: vec![base.clone()],
            },
        };
        assert!(store.authenticate_at(&token, now).is_some());
        assert!(
            store
                .authenticate_at(&token, now + Duration::seconds(1))
                .is_none()
        );
        let mut revoked = base;
        revoked.expires_at = None;
        revoked.revoked_at = Some(now);
        let store = TokenStore {
            file: CredentialFile {
                version: 1,
                credentials: vec![revoked],
            },
        };
        assert!(store.authenticate_at(&token, now).is_none());
    }

    #[test]
    fn rotation_has_bounded_ab_overlap_and_preserves_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let first = mint_credential(
            &path,
            Some("device:a"),
            &[TERMINAL_CONTROL_SCOPE.to_owned()],
            None,
        )
        .unwrap();
        let first_secret = secret(&first);
        let now = Utc::now() + Duration::seconds(1);
        let second = rotate_credential_at(
            &path,
            &first.id,
            Duration::minutes(5),
            now,
            AtomicWriteFault::None,
        )
        .unwrap();
        let second_secret = secret(&second);
        let store = TokenStore::load(&path).unwrap();
        assert_eq!(
            store
                .authenticate_at(&first_secret, now)
                .unwrap()
                .generation,
            1
        );
        let current = store.authenticate_at(&second_secret, now).unwrap();
        assert_eq!(current.id, first.id);
        assert_eq!(current.principal, "device:a");
        assert_eq!(current.generation, 2);
        assert!(
            store
                .authenticate_at(&first_secret, now + Duration::minutes(5))
                .is_none()
        );
        assert!(
            store
                .authenticate_at(&second_secret, now + Duration::minutes(5))
                .is_some()
        );
    }

    #[test]
    fn rotation_preserves_absolute_expiry_for_new_and_overlap_generations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let now = Utc::now() + Duration::seconds(1);
        let expiry = now + Duration::minutes(2);
        let first = mint_credential(
            &path,
            Some("device:expiring"),
            &[TERMINAL_CONTROL_SCOPE.to_owned()],
            Some(expiry),
        )
        .unwrap();
        let first_secret = secret(&first);
        let second = rotate_credential_at(
            &path,
            &first.id,
            Duration::minutes(5),
            now,
            AtomicWriteFault::None,
        )
        .unwrap();
        let second_secret = secret(&second);
        let store = TokenStore::load(&path).unwrap();

        assert!(store.authenticate_at(&first_secret, now).is_some());
        let current = store.authenticate_at(&second_secret, now).unwrap();
        assert_eq!(current.expires_at, Some(expiry));
        assert!(store.authenticate_at(&first_secret, expiry).is_none());
        assert!(store.authenticate_at(&second_secret, expiry).is_none());
    }

    #[test]
    fn rotation_rejects_an_already_expired_credential_without_changing_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let now = Utc::now() + Duration::minutes(1);
        let first = mint_credential(
            &path,
            Some("device:expired"),
            &[TERMINAL_CONTROL_SCOPE.to_owned()],
            Some(now - Duration::seconds(1)),
        )
        .unwrap();
        let before = fs::read(&path).unwrap();

        let result = rotate_credential_at(
            &path,
            &first.id,
            Duration::minutes(5),
            now,
            AtomicWriteFault::None,
        );

        assert!(matches!(result, Err(AuthError::CredentialExpired(id)) if id == first.id));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(TokenStore::load(&path).unwrap().len(), 1);
    }

    #[test]
    fn insecure_store_types_permissions_and_ownership_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let minted =
            mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        let bearer = secret(&minted);
        let live = ReloadingTokenStore::load(path.clone()).unwrap();
        assert!(live.verify(&bearer));

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(matches!(
            TokenStore::load(&path),
            Err(AuthError::InsecureStore(_))
        ));
        assert!(!live.verify(&bearer), "reload fails closed on unsafe mode");
        assert!(live.cached_is_empty(), "integrity failure clears the cache");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(live.verify(&bearer), "repair restores authentication");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o200)).unwrap();
        assert!(matches!(
            TokenStore::load(&path),
            Err(AuthError::InsecureStore(_))
        ));
        assert!(!live.verify(&bearer), "owner-unreadable stores fail closed");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert!(matches!(
            validate_store_metadata_for_uid(&path, &metadata, metadata.uid().saturating_add(1)),
            Err(AuthError::InsecureStore(_))
        ));

        let target = dir.path().join("target");
        fs::rename(&path, &target).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(matches!(
            TokenStore::load(&path),
            Err(AuthError::InsecureStore(_))
        ));
        assert!(!live.verify(&bearer), "reload fails closed on symlinks");

        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(matches!(
            TokenStore::load(&path),
            Err(AuthError::InsecureStore(_))
        ));
    }

    #[test]
    fn fault_injected_rotation_is_old_or_new_at_each_atomic_commit_stage() {
        for fault in [
            AtomicWriteFault::BeforeTempSync,
            AtomicWriteFault::AfterTempSyncBeforeRename,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("credentials");
            let first =
                mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
            let result = with_store_lock(&path, || {
                rotate_credential_at(&path, &first.id, Duration::minutes(5), Utc::now(), fault)
            });
            assert!(result.is_err());
            let store = TokenStore::load(&path).unwrap();
            assert!(store.verify(&secret(&first)));
            assert_eq!(
                store.len(),
                1,
                "pre-rename interruption preserves old store"
            );
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let first =
            mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        let rotated = with_store_lock(&path, || {
            rotate_credential_at(
                &path,
                &first.id,
                Duration::minutes(5),
                Utc::now(),
                AtomicWriteFault::AfterRenameBeforeDirectorySync,
            )
        })
        .unwrap();
        assert!(!rotated.is_durable(), "post-rename ambiguity is explicit");
        let store = TokenStore::load(&path).unwrap();
        assert!(
            store.verify(&secret(&rotated)),
            "renamed generation is visible"
        );
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn unknown_store_and_record_fields_are_denied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let minted =
            mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        value["unknown"] = serde_json::json!(true);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(matches!(
            TokenStore::load(&path),
            Err(AuthError::Malformed(_))
        ));

        value.as_object_mut().unwrap().remove("unknown");
        value["credentials"][0]["unknown"] = serde_json::json!(minted.id);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(matches!(
            TokenStore::load(&path),
            Err(AuthError::Malformed(_))
        ));
    }

    #[test]
    fn hostile_precreated_lock_and_untrusted_parent_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let lock_path = credential_lock_path(&path);
        let target = dir.path().join("attacker-lock-target");
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&target)
            .unwrap();
        std::os::unix::fs::symlink(&target, &lock_path).unwrap();
        assert!(matches!(
            mint_token(&path),
            Err(AuthError::InsecureStore(_))
        ));

        fs::remove_file(&lock_path).unwrap();
        fs::write(&lock_path, b"hostile").unwrap();
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            mint_token(&path),
            Err(AuthError::InsecureStore(_))
        ));

        fs::remove_file(&lock_path).unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o770)).unwrap();
        assert!(matches!(
            mint_token(&path),
            Err(AuthError::InsecureStore(_))
        ));
    }

    #[test]
    fn replacing_lock_path_cannot_split_concurrent_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        mint_token(&path).unwrap();

        let acquired = std::sync::Arc::new(std::sync::Barrier::new(2));
        let release = std::sync::Arc::new(std::sync::Barrier::new(2));
        let first_path = path.clone();
        let first_acquired = acquired.clone();
        let first_release = release.clone();
        let first = std::thread::spawn(move || {
            with_store_lock(&first_path, || {
                first_acquired.wait();
                first_release.wait();
                mint_credential_unlocked(
                    &first_path,
                    None,
                    &[TERMINAL_CONTROL_SCOPE.to_owned()],
                    None,
                )
            })
        });
        acquired.wait();

        let lock_path = credential_lock_path(&path);
        fs::rename(&lock_path, dir.path().join("displaced-lock")).unwrap();
        OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&lock_path)
            .unwrap();

        let (completed_tx, completed_rx) = std::sync::mpsc::channel();
        let second_path = path.clone();
        let second = std::thread::spawn(move || {
            let result = mint_token(&second_path);
            completed_tx.send(()).unwrap();
            result
        });
        assert!(
            completed_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "replacement lock inode must not admit a concurrent writer"
        );

        release.wait();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        assert_eq!(
            TokenStore::load(&path).unwrap().len(),
            3,
            "both serialized mutations survive lock-path replacement"
        );
    }

    #[test]
    fn credential_lock_child() {
        let Some(path) = std::env::var_os("PHUX_TEST_CREDENTIAL_STORE") else {
            return;
        };
        mint_credential(
            Path::new(&path),
            None,
            &[TERMINAL_CONTROL_SCOPE.to_owned()],
            None,
        )
        .unwrap();
    }

    #[test]
    fn concurrent_process_writers_do_not_lose_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let executable = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        for _ in 0..8 {
            children.push(
                std::process::Command::new(&executable)
                    .args(["--exact", "auth::tests::credential_lock_child"])
                    .env("PHUX_TEST_CREDENTIAL_STORE", &path)
                    .spawn()
                    .unwrap(),
            );
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        assert_eq!(TokenStore::load(&path).unwrap().len(), 8);
    }

    #[test]
    fn concurrent_rotation_cannot_resurrect_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let first =
            mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        let original = secret(&first);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let rotate_path = path.clone();
        let rotate_id = first.id.clone();
        let rotate_barrier = barrier.clone();
        let rotate = std::thread::spawn(move || {
            rotate_barrier.wait();
            rotate_credential(&rotate_path, &rotate_id, Duration::minutes(5))
        });
        let revoke_path = path.clone();
        let revoke_id = first.id;
        let revoke_barrier = barrier.clone();
        let revoke = std::thread::spawn(move || {
            revoke_barrier.wait();
            revoke_credential(&revoke_path, &revoke_id)
        });
        barrier.wait();
        let rotated = rotate.join().unwrap();
        revoke.join().unwrap().unwrap();
        let store = TokenStore::load(&path).unwrap();
        assert!(!store.verify(&original));
        if let Ok(rotated) = rotated {
            assert!(!store.verify(&secret(&rotated)));
        }
    }

    #[test]
    fn changed_malformed_generation_denies_instead_of_using_cached_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let first =
            mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        let store = ReloadingTokenStore::load(path.clone()).unwrap();
        let second =
            mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        assert!(store.verify(&secret(&second)));
        assert_eq!(store.reloads(), 1);
        fs::write(&path, "{broken").unwrap();
        assert!(!store.verify(&secret(&first)));
        assert!(store.cached_is_empty(), "malformed change clears the cache");
        assert_eq!(store.reloads(), 1, "failed reads do not commit a stamp");
    }

    #[test]
    fn generation_unstable_through_all_retries_denies_cached_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let minted =
            mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        let bearer = secret(&minted);
        let store = ReloadingTokenStore::load(path.clone()).unwrap();

        fs::write(&path, "{changed").unwrap();
        let changing_path = path;
        let accepted = store.with_current_observed(
            move |attempt| {
                let replacement = if attempt % 2 == 0 {
                    "{changed-again"
                } else {
                    "{changed"
                };
                fs::write(&changing_path, replacement).unwrap();
            },
            |snapshot| snapshot.verify(&bearer),
        );

        assert!(!accepted, "unstable changed state cannot retain old access");
        assert!(store.cached_is_empty(), "unstable change clears the cache");
        assert_eq!(store.reloads(), 0);
    }

    #[test]
    fn unchanged_store_is_statted_without_re_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let minted =
            mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        let bearer = secret(&minted);
        let store = ReloadingTokenStore::load(path).unwrap();
        for _ in 0..8 {
            assert!(store.verify(&bearer));
        }
        assert_eq!(store.reloads(), 0);
    }

    #[test]
    fn missing_generation_is_cached_while_creation_and_deletion_are_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let store = ReloadingTokenStore::load(path.clone()).unwrap();
        let absent_bearer = [0x55; TOKEN_LEN];

        for _ in 0..8 {
            assert!(!store.verify(&absent_bearer));
        }
        assert_eq!(
            store.reloads(),
            0,
            "an unchanged missing generation is not re-read"
        );

        let minted =
            mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        let bearer = secret(&minted);
        assert!(store.verify(&bearer), "later store creation is discovered");
        assert_eq!(store.reloads(), 1);

        fs::remove_file(&path).unwrap();
        assert!(!store.verify(&bearer), "store deletion revokes credentials");
        assert_eq!(store.reloads(), 2);
        assert!(!store.verify(&bearer));
        assert_eq!(
            store.reloads(),
            2,
            "the missing generation after deletion is cached too"
        );
    }

    #[test]
    fn stable_reads_reject_credentials_deleted_during_initialization_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let stale =
            mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        let stale_bearer = secret(&stale);

        // Reproduce the initialization race deterministically: the first read
        // sees the credential, then deletion lands before the generation probe
        // that the old load-then-probe implementation cached beside it.
        let init_path = path.clone();
        let store = ReloadingTokenStore::load_observed(path.clone(), move |attempt| {
            if attempt == 0 {
                fs::remove_file(&init_path).unwrap();
            }
        })
        .unwrap();
        assert!(store.is_empty());
        assert!(!store.verify(&stale_bearer));
        assert_eq!(store.reloads(), 0);

        for _ in 0..8 {
            assert!(!store.verify(&stale_bearer));
        }
        assert_eq!(store.reloads(), 0, "the stable absent result is cached");

        let current =
            mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        let current_bearer = secret(&current);
        assert!(
            store.verify(&current_bearer),
            "later creation is discovered"
        );
        assert_eq!(store.reloads(), 1);

        // Force another reload, then delete after its first file read. The
        // stable-read retry must commit the absent generation, not credentials
        // from the now-deleted generation.
        mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        let reload_path = path;
        let accepted = store.with_current_observed(
            move |attempt| {
                if attempt == 0 {
                    fs::remove_file(&reload_path).unwrap();
                }
            },
            |snapshot| snapshot.verify(&current_bearer),
        );
        assert!(!accepted, "reload cannot cache a deleted credential");
        assert_eq!(store.reloads(), 2);
        assert!(!store.verify(&current_bearer));
        assert_eq!(
            store.reloads(),
            2,
            "the absent generation produced by the retry remains cached"
        );
    }

    #[test]
    fn deleting_store_revokes_all_on_next_authentication() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let minted =
            mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        let bearer = secret(&minted);
        let store = ReloadingTokenStore::load(path.clone()).unwrap();
        assert!(store.verify(&bearer));
        fs::remove_file(path).unwrap();
        assert!(!store.verify(&bearer));
        assert!(store.is_empty());
    }

    #[test]
    fn revocation_affects_new_authentication_not_established_attestation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let minted = mint_credential(
            &path,
            Some("device:a"),
            &[TERMINAL_CONTROL_SCOPE.to_owned()],
            None,
        )
        .unwrap();
        let bearer = secret(&minted);
        let live = ReloadingTokenStore::load(path.clone()).unwrap();
        let established = live.authenticate(&bearer).unwrap();
        revoke_credential(&path, &minted.id).unwrap();
        assert!(
            live.authenticate(&bearer).is_none(),
            "next handshake is revoked"
        );
        assert_eq!(
            established.principal, "device:a",
            "established session retains its captured attestation"
        );
    }

    #[test]
    fn debug_output_never_contains_bearer_or_verifier() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials");
        let minted =
            mint_credential(&path, None, &[TERMINAL_CONTROL_SCOPE.to_owned()], None).unwrap();
        let raw = fs::read_to_string(&path).unwrap();
        let verifier = raw
            .split("sha256:")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        let output = format!(
            "{minted:?} {:?} {:?}",
            TokenStore::load(&path).unwrap(),
            ReloadingTokenStore::load(path).unwrap()
        );
        assert!(!output.contains(minted.secret()));
        assert!(!output.contains(verifier));
        assert!(output.contains("[REDACTED]"));
    }
}
