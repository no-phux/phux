//! Registry writes: registering, replacing, and revoking credentials.

use std::path::Path;

use chrono::Utc;

use super::{RegisteredCredential, RegistryFile, RegistryRecord, Revocation, WorkloadRegistry};
use crate::workload::store;
use crate::workload::{
    EnrolledFor, STORE_VERSION, WorkloadCredential, WorkloadError, credential_id,
    is_canonical_credential_id, validate_expiry, validate_satellite_name, validate_scopes,
};

impl WorkloadRegistry {
    /// Register a public key with a closed scope ceiling and persist the
    /// updated snapshot under the registry lock. The returned id is stable
    /// across certificate renewal.
    ///
    /// # Errors
    ///
    /// An invalid scope or expiry, a key that is already enrolled, or any
    /// failure to read or atomically replace the registry.
    pub fn register(
        path: &Path,
        public_key: &[u8],
        scopes: Vec<String>,
        expires_at: Option<i64>,
    ) -> Result<RegisteredCredential, WorkloadError> {
        Self::register_replacing(path, public_key, scopes, expires_at, None, None)
    }

    /// [`Self::register`], and in the same locked write revoke `replaces`:
    /// re-enrollment swaps a client's credential in one registry generation,
    /// so no snapshot admits both keys or neither. A `replaces` the registry
    /// does not hold (or already revoked) is not an error: there is nothing
    /// left to swap out.
    ///
    /// # Errors
    ///
    /// As [`Self::register`], and [`WorkloadError::InvalidCredentialId`] for
    /// a `replaces` not in the canonical spelling.
    ///
    /// `enrolled_for` is recorded when named; `None` leaves the record to
    /// [`WorkloadCredential::enrolled_for`]'s inference.
    pub fn register_replacing(
        path: &Path,
        public_key: &[u8],
        scopes: Vec<String>,
        expires_at: Option<i64>,
        replaces: Option<&str>,
        enrolled_for: Option<EnrolledFor>,
    ) -> Result<RegisteredCredential, WorkloadError> {
        if replaces.is_some_and(|id| !is_canonical_credential_id(id)) {
            return Err(WorkloadError::InvalidCredentialId);
        }
        if public_key.is_empty() {
            return Err(WorkloadError::Malformed(
                "public key must be non-empty".to_owned(),
            ));
        }
        validate_scopes(&scopes)?;
        if let Some(expires_at) = expires_at {
            validate_expiry(expires_at)?;
        }
        let id = credential_id(public_key);
        store::with_lock(path, |held| {
            let mut registry = Self::load(path)?;
            if registry
                .credentials
                .iter()
                .any(|credential| credential.id == id)
            {
                return Err(WorkloadError::AlreadyRegistered(id.clone()));
            }
            let now = Utc::now().timestamp();
            let revoked_predecessor = replaces
                .and_then(|old| {
                    registry
                        .credentials
                        .iter_mut()
                        .find(|credential| credential.id == old && credential.revoked_at.is_none())
                })
                .map(|old| old.revoked_at = Some(now))
                .is_some();
            registry.credentials.push(WorkloadCredential {
                id: id.clone(),
                public_key: public_key.to_vec(),
                scopes,
                expires_at,
                revoked_at: None,
                recorded_for: enrolled_for,
                satellite: None,
            });
            registry.commit(path, held)?;
            Ok(RegisteredCredential {
                id: id.clone(),
                generation: registry.generation,
                replaced: revoked_predecessor,
            })
        })
    }

    /// Register `public_key` as satellite `name`'s credential (ADR-0182) and,
    /// in the same locked write, revoke every other active credential of that
    /// name: one satellite name has one live credential.
    ///
    /// # Errors
    ///
    /// An invalid name or expiry, a key that is already enrolled, or any
    /// failure to read or atomically replace the registry.
    pub fn register_satellite(
        path: &Path,
        public_key: &[u8],
        name: &str,
        expires_at: i64,
    ) -> Result<RegisteredCredential, WorkloadError> {
        validate_satellite_name(name)?;
        validate_expiry(expires_at)?;
        if public_key.is_empty() {
            return Err(WorkloadError::Malformed(
                "public key must be non-empty".to_owned(),
            ));
        }
        let id = credential_id(public_key);
        store::with_lock(path, |held| {
            let mut registry = Self::load(path)?;
            if registry
                .credentials
                .iter()
                .any(|credential| credential.id == id)
            {
                return Err(WorkloadError::AlreadyRegistered(id.clone()));
            }
            let now = Utc::now().timestamp();
            let replaced = registry.revoke_satellite_records(name, now) > 0;
            registry.credentials.push(WorkloadCredential {
                id: id.clone(),
                public_key: public_key.to_vec(),
                scopes: Vec::new(),
                expires_at: Some(expires_at),
                revoked_at: None,
                recorded_for: Some(EnrolledFor::Automation),
                satellite: Some(name.to_owned()),
            });
            registry.commit(path, held)?;
            Ok(RegisteredCredential {
                id: id.clone(),
                generation: registry.generation,
                replaced,
            })
        })
    }

    /// Revoke every active credential of satellite `name` (`phux host rm`).
    /// Returns how many were revoked; nothing is written when none was.
    ///
    /// # Errors
    ///
    /// Any failure to read or atomically replace the registry.
    pub fn revoke_satellite(path: &Path, name: &str) -> Result<usize, WorkloadError> {
        store::with_lock(path, |held| {
            let mut registry = Self::load(path)?;
            let revoked = registry.revoke_satellite_records(name, Utc::now().timestamp());
            if revoked > 0 {
                registry.commit(path, held)?;
            }
            Ok(revoked)
        })
    }

    fn revoke_satellite_records(&mut self, name: &str, now: i64) -> usize {
        let mut revoked = 0;
        for credential in &mut self.credentials {
            if credential.satellite.as_deref() == Some(name) && credential.revoked_at.is_none() {
                credential.revoked_at = Some(now);
                revoked += 1;
            }
        }
        revoked
    }

    /// Mark a credential revoked from now on. The record stays, so the key
    /// cannot be silently re-enrolled; a running server stops admitting it at
    /// its next registry observation.
    ///
    /// # Errors
    ///
    /// A non-canonical or unknown id, or any failure to read or atomically
    /// replace the registry.
    pub fn revoke(path: &Path, id: &str) -> Result<Revocation, WorkloadError> {
        if !is_canonical_credential_id(id) {
            return Err(WorkloadError::InvalidCredentialId);
        }
        store::with_lock(path, |held| {
            let mut registry = Self::load(path)?;
            let now = Utc::now().timestamp();
            let credential = registry
                .credentials
                .iter_mut()
                .find(|credential| credential.id == id)
                .ok_or_else(|| WorkloadError::NotFound(id.to_owned()))?;
            if let Some(revoked_at) = credential.revoked_at {
                return Ok(Revocation {
                    generation: registry.generation,
                    revoked_at,
                    newly_revoked: false,
                });
            }
            credential.revoked_at = Some(now);
            registry.commit(path, held)?;
            Ok(Revocation {
                generation: registry.generation,
                revoked_at: now,
                newly_revoked: true,
            })
        })
    }

    /// Advance the generation and atomically replace the file. `held` is the
    /// caller's registry lock.
    pub(super) fn commit(
        &mut self,
        path: &Path,
        held: &store::HeldLock<'_>,
    ) -> Result<(), WorkloadError> {
        if self.instance.is_none() {
            self.instance = Some(Self::fresh_instance_id()?);
        }
        self.generation = self.generation.saturating_add(1);
        let records = self
            .credentials
            .iter()
            .map(|credential| RegistryRecord {
                id: credential.id.clone(),
                public_key: hex::encode(&credential.public_key),
                scopes: credential.scopes.clone(),
                expires_at: credential.expires_at,
                revoked_at: credential.revoked_at,
                principal: credential.recorded_for,
                satellite: credential.satellite.clone(),
            })
            .collect();
        let mut bytes = serde_json::to_vec_pretty(&RegistryFile {
            version: STORE_VERSION,
            instance: self.instance.clone(),
            generation: self.generation,
            credentials: records,
        })
        .map_err(|error| WorkloadError::Malformed(error.to_string()))?;
        bytes.push(b'\n');
        store::atomic_replace(path, &bytes, held)
    }
}
