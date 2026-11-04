//! The registry file format, validated snapshots, and credential lookup.

use std::io;
use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use super::store::{self, FileRole};
use super::{
    EnrolledFor, STORE_VERSION, WorkloadCredential, WorkloadError, credential_id, decode_lower_hex,
    subject_public_key_info, validate_satellite_name, validate_scopes,
};

mod paired;
mod registration;

/// Result of an explicit legacy paired-device permission repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairedDeviceRepair {
    /// Registry generation after the operation.
    pub generation: u64,
    /// Whether the legacy ceiling was replaced; false on a repeat.
    pub updated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    version: u32,
    /// Random id minted by the file's first write and kept thereafter; with
    /// `generation` it names one registry state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instance: Option<String>,
    /// Advanced by every committed write; absent in files that predate it.
    #[serde(default)]
    generation: u64,
    credentials: Vec<RegistryRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryRecord {
    id: String,
    public_key: String,
    scopes: Vec<String>,
    expires_at: Option<i64>,
    revoked_at: Option<i64>,
    /// Who the credential was enrolled for; absent in records that predate
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    principal: Option<EnrolledFor>,
    /// The satellite name, on a satellite credential only (ADR-0182), so a
    /// registry with none stays readable by readers that predate it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    satellite: Option<String>,
}

/// A validated snapshot of the workload-key registry.
#[derive(Debug, Clone)]
pub struct WorkloadRegistry {
    credentials: Vec<WorkloadCredential>,
    instance: Option<String>,
    generation: u64,
}

/// A credential committed by [`WorkloadRegistry::register`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredCredential {
    /// Its public-key-derived id.
    pub id: String,
    /// The registry generation the commit produced.
    pub generation: u64,
    /// Whether the commit also revoked the credential it replaced
    /// ([`WorkloadRegistry::register_replacing`]); `false` when none was named,
    /// or the named one was absent or already revoked.
    pub replaced: bool,
}

/// The result of [`WorkloadRegistry::revoke`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Revocation {
    /// The registry generation after the call.
    pub generation: u64,
    /// Unix time the credential is revoked from.
    pub revoked_at: i64,
    /// `false` when it was already revoked (nothing was written).
    pub newly_revoked: bool,
}

impl WorkloadRegistry {
    /// Load a registry strictly: an owner-only, stable, valid file. A missing
    /// file is an empty authority snapshot.
    ///
    /// # Errors
    ///
    /// Any [`WorkloadError`] reading or validating the file.
    pub fn load(path: &Path) -> Result<Self, WorkloadError> {
        match store::read_stable(path, FileRole::Registry)? {
            None => Ok(Self::empty()),
            Some((_, raw)) => Self::from_bytes(&raw),
        }
    }

    /// Parse and validate one registry image. One invalid record makes the
    /// whole image malformed (`docs/remote.md#who-can-connect`).
    ///
    /// # Errors
    ///
    /// [`WorkloadError::Malformed`] or [`WorkloadError::InvalidScope`].
    pub fn from_bytes(raw: &[u8]) -> Result<Self, WorkloadError> {
        let file: RegistryFile = serde_json::from_slice(raw)
            .map_err(|error| WorkloadError::Malformed(error.to_string()))?;
        if file.version != STORE_VERSION {
            return Err(WorkloadError::Malformed(format!(
                "unsupported version {}",
                file.version
            )));
        }
        if file
            .instance
            .as_deref()
            .is_some_and(|id| !Self::is_instance_id(id))
        {
            return Err(WorkloadError::Malformed(
                "the registry instance id is not 32 lowercase hexadecimal digits".to_owned(),
            ));
        }
        let mut credentials: Vec<WorkloadCredential> = Vec::with_capacity(file.credentials.len());
        for record in &file.credentials {
            let credential = credential_from_record(record)?;
            if credentials.iter().any(|seen| seen.id == credential.id) {
                return Err(WorkloadError::Malformed(
                    "a credential id appears twice".to_owned(),
                ));
            }
            credentials.push(credential);
        }
        Ok(Self {
            credentials,
            instance: file.instance,
            generation: file.generation,
        })
    }

    /// An empty authority snapshot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            credentials: Vec::new(),
            instance: None,
            generation: 0,
        }
    }

    /// The generation of the registry file this snapshot was read from
    /// (`0` for an empty or pre-generation snapshot). Meaningful only
    /// together with [`Self::instance_id`].
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// The file's instance id (`None` if empty or not yet rewritten). Key
    /// derived state on `(instance_id, generation)`.
    #[must_use]
    pub fn instance_id(&self) -> Option<&str> {
        self.instance.as_deref()
    }

    /// A fresh random instance id: 16 bytes, as lowercase hex.
    fn fresh_instance_id() -> Result<String, WorkloadError> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(hex::encode(bytes))
    }

    fn is_instance_id(id: &str) -> bool {
        id.len() == 32
            && id
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    }

    /// Every record, active or not, in file order.
    #[must_use]
    pub fn credentials(&self) -> &[WorkloadCredential] {
        &self.credentials
    }

    /// Find an active terminal-endpoint credential by its public-key-derived
    /// id. A satellite credential is never one (ADR-0182): every terminal
    /// admission path reads the registry through here, so it is refused on
    /// all of them alike.
    #[must_use]
    pub fn lookup(&self, id: &str) -> Option<&WorkloadCredential> {
        self.find_active(id)
            .filter(|credential| credential.satellite.is_none())
    }

    /// Find an active terminal-endpoint credential by a verified TLS leaf's
    /// public key (see [`Self::lookup`]).
    #[must_use]
    pub fn lookup_certificate(&self, certificate: &[u8]) -> Option<&WorkloadCredential> {
        let public_key = subject_public_key_info(certificate).ok()?;
        self.lookup(&credential_id(&public_key))
    }

    /// Find an active satellite credential by its id (ADR-0182).
    #[must_use]
    pub fn lookup_satellite(&self, id: &str) -> Option<&WorkloadCredential> {
        self.find_active(id)
            .filter(|credential| credential.satellite.is_some())
    }

    /// Find an active satellite credential by a verified TLS leaf's public
    /// key (ADR-0182).
    #[must_use]
    pub fn lookup_satellite_certificate(&self, certificate: &[u8]) -> Option<&WorkloadCredential> {
        let public_key = subject_public_key_info(certificate).ok()?;
        self.lookup_satellite(&credential_id(&public_key))
    }

    fn find_active(&self, id: &str) -> Option<&WorkloadCredential> {
        let now = Utc::now().timestamp();
        self.credentials.iter().find(|credential| {
            bool::from(credential.id.as_bytes().ct_eq(id.as_bytes()))
                && credential.is_active_at(now)
        })
    }

    /// Number of records in this validated snapshot.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.credentials.len()
    }

    /// Whether this snapshot contains no records.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.credentials.is_empty()
    }
}

fn credential_from_record(record: &RegistryRecord) -> Result<WorkloadCredential, WorkloadError> {
    let public_key = decode_lower_hex(&record.public_key)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            WorkloadError::Malformed("public_key is not non-empty lowercase hex".to_owned())
        })?;
    if record.id != credential_id(&public_key) {
        return Err(WorkloadError::Malformed(
            "a credential id does not match its public key".to_owned(),
        ));
    }
    match &record.satellite {
        // A satellite credential grants no terminal authority (ADR-0182).
        Some(name) => {
            validate_satellite_name(name)?;
            if !record.scopes.is_empty() {
                return Err(WorkloadError::Malformed(
                    "a satellite credential carries no scope ceiling".to_owned(),
                ));
            }
        }
        None => validate_scopes(&record.scopes)?,
    }
    Ok(WorkloadCredential {
        id: record.id.clone(),
        public_key,
        scopes: record.scopes.clone(),
        expires_at: record.expires_at,
        revoked_at: record.revoked_at,
        recorded_for: record.principal,
        satellite: record.satellite.clone(),
    })
}

#[cfg(test)]
mod frozen {
    #[test]
    fn workload_keys() {
        phux_frozen::json::<super::RegistryFile>("workload-keys").check();
    }
}
