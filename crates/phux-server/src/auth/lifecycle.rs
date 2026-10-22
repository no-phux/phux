//! Minting, rotating, listing, pruning, and revoking credentials.

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::{
    AtomicWriteFault, AuthError, CredentialFile, CredentialRecord, CredentialSummary,
    MintedCredential, PruneOutcome, TERMINAL_CONTROL_SCOPE, TOKEN_LEN, TokenStore, atomic_write,
    atomic_write_with_fault, decode_verifier, verifier, with_store_lock,
};

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

pub(super) fn mint_credential_unlocked(
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
        last_seen: None,
    });
    let commit = atomic_write(path, &file)?;
    Ok(MintedCredential {
        id,
        generation: 1,
        secret: hex::encode(secret),
        durable: commit.is_durable(),
    })
}

/// Rotate a credential with a bounded overlap, atomically.
pub fn rotate_credential(
    path: &Path,
    id: &str,
    overlap: Duration,
) -> Result<MintedCredential, AuthError> {
    with_store_lock(path, || {
        rotate_credential_at(path, id, overlap, Utc::now(), AtomicWriteFault::None)
    })
}

pub(super) fn rotate_credential_at(
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
    getrandom::fill(&mut secret)?;
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
        last_seen: None,
    });
    let commit = atomic_write_with_fault(path, &file, fault)?;
    Ok(MintedCredential {
        id: id.to_owned(),
        generation,
        secret: hex::encode(secret),
        durable: commit.is_durable(),
    })
}

/// Mint a generation-one terminal credential.
///
/// `replacing` revokes any live credential whose bearer matches that hex
/// secret in the same rewrite. `expires_at` bounds later admission; `None`
/// leaves the credential valid until it is revoked. The bearer is returned
/// once and stored only as a verifier.
pub fn mint_terminal_credential(
    path: &Path,
    replacing: Option<&str>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<MintedCredential, AuthError> {
    with_store_lock(path, || {
        let previous = replacing.map(decode_secret).transpose()?;
        let mut file = load_file_for_update(path)?;
        let now = Utc::now();
        if let Some(previous) = previous
            && let Some(id) = credential_id_for_secret(&file, &previous)
        {
            for record in file.credentials.iter_mut().filter(|record| record.id == id) {
                if record.revoked_at.is_none() {
                    record.revoked_at = Some(now);
                }
            }
        }
        let (id, secret) = random_identity_and_secret()?;
        file.credentials.push(CredentialRecord {
            id: id.clone(),
            verifier: verifier(&secret),
            principal: format!("remote-consumer:{id}"),
            scopes: vec![TERMINAL_CONTROL_SCOPE.to_owned()],
            issued_at: now,
            expires_at,
            revoked_at: None,
            generation: 1,
            last_seen: None,
        });
        let commit = atomic_write(path, &file)?;
        Ok(MintedCredential {
            id,
            generation: 1,
            secret: hex::encode(secret),
            durable: commit.is_durable(),
        })
    })
}

/// List credentials aggregated by id (mint time, last seen, revoked).
pub fn list_credentials(path: &Path) -> Result<Vec<CredentialSummary>, AuthError> {
    let store = TokenStore::load(path)?;
    Ok(summarize_credentials(&store.file))
}

/// Revoke every live credential unused for `unused_for` (`last_seen`, else
/// `issued_at`).
pub fn prune_unused(path: &Path, unused_for: Duration) -> Result<PruneOutcome, AuthError> {
    with_store_lock(path, || {
        let mut file = load_file_for_update(path)?;
        let now = Utc::now();
        let cutoff = now - unused_for.max(Duration::zero());
        let mut revoked_ids = Vec::new();
        for summary in summarize_credentials(&file) {
            if summary.revoked {
                continue;
            }
            let activity = summary.last_seen.unwrap_or(summary.issued_at);
            if activity > cutoff {
                continue;
            }
            for record in file
                .credentials
                .iter_mut()
                .filter(|record| record.id == summary.id)
            {
                if record.revoked_at.is_none() {
                    record.revoked_at = Some(now);
                }
            }
            revoked_ids.push(summary.id);
        }
        if revoked_ids.is_empty() {
            return Ok(PruneOutcome {
                revoked_ids,
                durable: true,
            });
        }
        let commit = atomic_write(path, &file)?;
        Ok(PruneOutcome {
            revoked_ids,
            durable: commit.is_durable(),
        })
    })
}

fn summarize_credentials(file: &CredentialFile) -> Vec<CredentialSummary> {
    let mut by_id: std::collections::BTreeMap<String, CredentialSummary> =
        std::collections::BTreeMap::new();
    for record in &file.credentials {
        let entry = by_id
            .entry(record.id.clone())
            .or_insert_with(|| CredentialSummary {
                id: record.id.clone(),
                issued_at: record.issued_at,
                last_seen: record.last_seen,
                revoked: record.revoked_at.is_some(),
                generation: record.generation,
            });
        if record.issued_at < entry.issued_at {
            entry.issued_at = record.issued_at;
        }
        entry.last_seen = match (entry.last_seen, record.last_seen) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        entry.revoked &= record.revoked_at.is_some();
        entry.generation = entry.generation.max(record.generation);
    }
    by_id.into_values().collect()
}

fn credential_id_for_secret(file: &CredentialFile, secret: &[u8; TOKEN_LEN]) -> Option<String> {
    let candidate = Sha256::digest(secret);
    let mut matched = None;
    for record in &file.credentials {
        let Ok(verifier) = decode_verifier(&record.verifier) else {
            continue;
        };
        if bool::from(verifier.ct_eq(candidate.as_slice())) {
            matched = Some(record.id.clone());
        }
    }
    matched
}

/// Record a successful authentication for `id`: best effort, at most once
/// a minute per credential.
pub fn record_credential_seen(path: &Path, id: &str) {
    let _ = with_store_lock(path, || {
        record_credential_seen_unlocked(path, id, Utc::now())
    });
}

fn record_credential_seen_unlocked(
    path: &Path,
    id: &str,
    now: DateTime<Utc>,
) -> Result<(), AuthError> {
    let mut file = load_file_for_update(path)?;
    let Some(latest) = file
        .credentials
        .iter()
        .filter(|record| record.id == id && record.revoked_at.is_none())
        .max_by_key(|record| record.generation)
        .map(|record| record.generation)
    else {
        return Ok(());
    };
    let Some(record) = file
        .credentials
        .iter_mut()
        .find(|record| record.id == id && record.generation == latest)
    else {
        return Ok(());
    };
    if record
        .last_seen
        .is_some_and(|seen| now - seen < Duration::seconds(60))
    {
        return Ok(());
    }
    record.last_seen = Some(now);
    let _ = atomic_write(path, &file)?;
    Ok(())
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

fn load_file_for_update(path: &Path) -> Result<CredentialFile, AuthError> {
    Ok(TokenStore::load(path)?.file)
}

pub(super) fn random_identity_and_secret() -> Result<(String, [u8; TOKEN_LEN]), AuthError> {
    let mut id = [0u8; 16];
    let mut secret = [0u8; TOKEN_LEN];
    getrandom::fill(&mut id)?;
    getrandom::fill(&mut secret)?;
    Ok((hex::encode(id), secret))
}

fn decode_secret(encoded: &str) -> Result<[u8; TOKEN_LEN], AuthError> {
    let bytes =
        hex::decode(encoded).map_err(|_| AuthError::Malformed("token is not hex".to_owned()))?;
    <[u8; TOKEN_LEN]>::try_from(bytes.as_slice())
        .map_err(|_| AuthError::Malformed("token has wrong length".to_owned()))
}

/// Outcome of an atomic store replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitOutcome {
    pub(super) durable: bool,
}

impl CommitOutcome {
    /// Whether both file contents and the containing directory entry reached
    /// stable storage. A false result is already visible and must not be retried.
    #[must_use]
    pub const fn is_durable(self) -> bool {
        self.durable
    }
}
