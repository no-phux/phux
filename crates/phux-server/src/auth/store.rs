//! The loaded credential store and the checks a store file must pass.

use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::{
    AuthError, AuthenticatedCredential, CredentialFile, STORE_VERSION, Standing, TOKEN_LEN,
    VERIFIER_PREFIX,
};

/// A parsed snapshot of the current structured credential store.
#[derive(Clone, Default)]
pub struct TokenStore {
    pub(super) file: CredentialFile,
}

impl std::fmt::Debug for TokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenStore")
            .field("credentials", &self.file.credentials.len())
            .finish()
    }
}

impl TokenStore {
    /// Load a versioned store. Missing files are empty; a store of legacy
    /// token lines is refused as [`AuthError::LegacyStore`].
    pub fn load(path: &Path) -> Result<Self, AuthError> {
        let raw = read_secure_store(path)?.unwrap_or_default();
        if raw.trim().is_empty() {
            return Ok(Self {
                file: CredentialFile::default(),
            });
        }
        if !raw.trim_start().starts_with('{') {
            return Err(AuthError::LegacyStore);
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

    pub(super) fn authenticate_at(
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
                    registry_instance: None,
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

    /// The standing of credential `id` at `generation`: absent or revoked
    /// is [`Standing::Revoked`].
    pub(crate) fn standing_at(&self, id: &str, generation: u64, now: DateTime<Utc>) -> Standing {
        let Some(record) = self
            .file
            .credentials
            .iter()
            .find(|record| record.id == id && record.generation == generation)
        else {
            return Standing::Revoked;
        };
        if record.revoked_at.is_some() {
            return Standing::Revoked;
        }
        match record.expires_at {
            Some(expiry) if now >= expiry => Standing::Expired,
            expires_at => Standing::Active { expires_at },
        }
    }
}

pub(super) fn validate_store_metadata(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), AuthError> {
    validate_store_metadata_for_uid(path, metadata, rustix::process::geteuid().as_raw())
}

pub(super) fn validate_store_metadata_for_uid(
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

pub(super) fn verifier(secret: &[u8]) -> String {
    format!("{VERIFIER_PREFIX}{}", hex::encode(Sha256::digest(secret)))
}

pub(super) fn decode_verifier(encoded: &str) -> Result<[u8; 32], AuthError> {
    let hex = encoded
        .strip_prefix(VERIFIER_PREFIX)
        .ok_or_else(|| AuthError::Malformed("credential verifier must use sha256".to_owned()))?;
    let bytes = hex::decode(hex)
        .map_err(|_| AuthError::Malformed("credential verifier is not hex".to_owned()))?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| AuthError::Malformed("credential verifier has wrong length".to_owned()))
}
