//! mTLS workload authority material and registry (ADR-0116).
//!
//! TLS proves possession of a client private key; this module owns the local
//! authority that decides which public keys are admitted and what their scope
//! ceiling is.  The certificate authority is deliberately separate from the
//! server leaf certificate: clients pin the CA fingerprint, while leaf
//! certificates may be renewed without changing workload identity.
//!
//! Persistence follows `workload-auth.md` §2: every file is owner-owned,
//! no-follow-opened, and replaced under an owner-controlled lock through a
//! synced temporary file and an atomic rename (the private `store` module).
//! The registry carries a generation that every write advances; a running
//! server observes each new generation without a restart
//! ([`ReloadingWorkloadRegistry`]).
//!
//! Enrollment (`phux workload add-key`) accepts public material only — a
//! client certificate issued by this authority, or a certificate signing
//! request it signs — never a private key ([`ClientMaterial`]). No type here
//! holds private key bytes past the call that uses them, and no diagnostic
//! names the CA private-key path.

mod material;
mod reload;
mod store;

pub use material::{ClientMaterial, MAX_MATERIAL_BYTES, MaterialError};
pub use phux_protocol::scope::ScopeGrammarError;
use phux_protocol::scope::{EffectiveScopeSet, ScopeGrant, Selector, TerminalScopeSet};
pub use reload::ReloadingWorkloadRegistry;

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Utc};
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, PublicKeyData,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, CertificateSigningRequestDer, UnixTime};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use store::FileRole;

const STORE_VERSION: u32 = 1;

/// Prefix of the canonical credential-id and fingerprint spelling.
const DIGEST_PREFIX: &str = "sha256:";

/// Latest expiry a credential may carry, as seconds from now (20 years).
/// Also keeps an issued certificate's validity inside X.509's date range.
pub const MAX_EXPIRY_SECONDS: i64 = 20 * 365 * 24 * 60 * 60;

/// Errors from provisioning, enrolling into, or loading workload authority
/// material. No variant carries key bytes or the CA private-key path.
#[derive(Debug, thiserror::Error)]
pub enum WorkloadError {
    /// Filesystem operation failed.
    #[error("workload authority io: {0}")]
    Io(#[from] io::Error),
    /// Certificate generation or signing failed.
    #[error("workload authority certificate: {0}")]
    Certificate(#[from] rcgen::Error),
    /// A persisted registry is malformed or violates its invariants.
    #[error("malformed workload registry: {0}")]
    Malformed(String),
    /// The persisted CA certificate or key is unusable.
    #[error("malformed workload authority: {0}")]
    MalformedAuthority(&'static str),
    /// One half of the persistent CA pair exists.
    #[error(
        "partial workload authority pair: the CA {present} exists but its {missing} does not; remove it deliberately to mint a new authority (every enrolled client must re-enroll)"
    )]
    PartialPair {
        /// The half that survived.
        present: &'static str,
        /// The half that is absent.
        missing: &'static str,
    },
    /// A file or directory failed its ownership, type, or mode check.
    #[error("workload {file} {reason}")]
    Insecure {
        /// The role of the file (never its path).
        file: &'static str,
        /// The rule it broke.
        reason: &'static str,
    },
    /// The file changed during every bounded read attempt.
    #[error("a workload authority file kept changing while it was read")]
    Unstable,
    /// A scope string breaks the registry grammar. `index` is 1-based.
    #[error("workload scope {index}: {reason}")]
    InvalidScope {
        /// Position of the scope among those supplied, counting from 1.
        index: usize,
        /// The rule it broke.
        reason: ScopeGrammarError,
    },
    /// A persisted scope names a session (`group:`) or Terminal
    /// (`terminal:`) id, which restarts with the server. `index` is 1-based.
    #[error(
        "workload scope {index}: group: and terminal: selectors name ids that restart with the server, so a registry cannot hold them"
    )]
    UnstableSelector {
        /// Position of the scope among those supplied, counting from 1.
        index: usize,
    },
    /// A credential was supplied with no scope at all.
    #[error("a workload credential needs at least one scope")]
    NoScopes,
    /// An expiry at or before now, or beyond [`MAX_EXPIRY_SECONDS`].
    #[error("a workload credential expiry must be later than now and at most 20 years away")]
    InvalidExpiry,
    /// A credential id that is not in the canonical spelling.
    #[error("a workload credential id is `sha256:` followed by 64 lowercase hexadecimal digits")]
    InvalidCredentialId,
    /// The public key is already enrolled (revoked or not).
    #[error("workload credential {0} is already enrolled; enroll a new key instead")]
    AlreadyRegistered(String),
    /// No enrolled credential has this id.
    #[error("no workload credential {0} is enrolled")]
    NotFound(String),
    /// Enrollment needs a CA that has not been initialized.
    #[error("no workload authority exists yet; run `phux workload authority --init`")]
    AuthorityMissing,
    /// A supplied certificate does not verify against this authority.
    #[error("the certificate does not chain to this workload authority or is not currently valid")]
    NotIssuedByAuthority,
    /// The supplied enrollment material was refused.
    #[error(transparent)]
    Material(#[from] MaterialError),
}

/// Where workload authority material lives. `PHUX_WORKLOAD_CA`,
/// `PHUX_WORKLOAD_CA_KEY`, and `PHUX_WORKLOAD_KEYS` select non-default
/// locations; they name files and never carry their contents.
#[derive(Clone)]
pub struct WorkloadPaths {
    /// CA certificate (public).
    pub ca_cert: PathBuf,
    /// CA private key (secret; never printed).
    pub ca_key: PathBuf,
    /// Workload-key registry.
    pub registry: PathBuf,
}

impl std::fmt::Debug for WorkloadPaths {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkloadPaths")
            .field("ca_cert", &self.ca_cert)
            .field("ca_key", &"[withheld]")
            .field("registry", &self.registry)
            .finish()
    }
}

impl WorkloadPaths {
    /// The configured locations: each environment override, else the
    /// default under the state directory.
    #[must_use]
    pub fn from_env() -> Self {
        let path = |var: &str, default: fn() -> PathBuf| {
            std::env::var_os(var).map_or_else(default, PathBuf::from)
        };
        Self {
            ca_cert: path("PHUX_WORKLOAD_CA", default_ca_cert_path),
            ca_key: path("PHUX_WORKLOAD_CA_KEY", default_ca_key_path),
            registry: path("PHUX_WORKLOAD_KEYS", default_registry_path),
        }
    }
}

/// A workload credential's canonical scope ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkloadCredential {
    /// `sha256:` identifier derived from the raw public key.
    pub id: String,
    /// Raw public key (DER `SubjectPublicKeyInfo`), lowercase hex on disk.
    pub public_key: Vec<u8>,
    /// Scope strings in the registry grammar
    /// ([`phux_protocol::scope::ScopeGrant::parse`]); see [`Self::ceiling`].
    pub scopes: Vec<String>,
    /// Absolute Unix expiry, if any.
    pub expires_at: Option<i64>,
    /// Revocation time, if revoked.
    pub revoked_at: Option<i64>,
}

impl WorkloadCredential {
    /// Neither revoked nor expired at Unix time `now`.
    #[must_use]
    pub fn is_active_at(&self, now: i64) -> bool {
        self.revoked_at.is_none() && self.expires_at.is_none_or(|expires| expires > now)
    }

    /// The scope ceiling as one canonical set (`workload-auth.md` §5).
    ///
    /// # Errors
    ///
    /// As [`validate_scopes`]. A record in a loaded snapshot always parses,
    /// because loading validated it.
    pub fn ceiling(&self) -> Result<TerminalScopeSet, WorkloadError> {
        parse_ceiling(&self.scopes)
    }

    /// The connection attestation for a TLS peer admitted under this
    /// credential from `registry`, stamped with that snapshot's instance id
    /// and generation.
    #[must_use]
    pub fn authenticated(
        &self,
        registry: &WorkloadRegistry,
    ) -> crate::auth::AuthenticatedCredential {
        crate::auth::AuthenticatedCredential {
            id: self.id.clone(),
            principal: self.id.clone(),
            scopes: self.scopes.clone(),
            issued_at: Utc::now(),
            expires_at: self
                .expires_at
                .and_then(|seconds| DateTime::from_timestamp(seconds, 0)),
            generation: registry.generation(),
            registry_instance: registry.instance_id().map(str::to_owned),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    version: u32,
    /// Random id minted by the first write of this file and kept by every
    /// later one. With `generation` it names one registry state: a deleted
    /// and recreated file restarts its generation under a new instance.
    /// Absent in files that predate it until their next write.
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
    /// whole image malformed (`workload-auth.md` §2).
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

    /// The registry file's random instance id, minted by its first write and
    /// kept by every later one; `None` for an empty snapshot or a file not
    /// yet rewritten. Key anything derived from a snapshot on
    /// `(instance_id, generation)`, never on the generation alone: a deleted
    /// and recreated registry restarts its generation under a new instance.
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

    /// Find an active credential by its public-key-derived id.
    #[must_use]
    pub fn lookup(&self, id: &str) -> Option<&WorkloadCredential> {
        let now = Utc::now().timestamp();
        self.credentials.iter().find(|credential| {
            bool::from(credential.id.as_bytes().ct_eq(id.as_bytes()))
                && credential.is_active_at(now)
        })
    }

    /// Find an active credential from a TLS leaf certificate's DER bytes.
    /// rustls has already verified the chain; this method only extracts the
    /// stable `SubjectPublicKeyInfo` used as the registry identity.
    #[must_use]
    pub fn lookup_certificate(&self, certificate: &[u8]) -> Option<&WorkloadCredential> {
        let public_key = subject_public_key_info(certificate).ok()?;
        self.lookup(&credential_id(&public_key))
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
            registry.credentials.push(WorkloadCredential {
                id: id.clone(),
                public_key: public_key.to_vec(),
                scopes,
                expires_at,
                revoked_at: None,
            });
            registry.commit(path, held)?;
            Ok(RegisteredCredential {
                id: id.clone(),
                generation: registry.generation,
            })
        })
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
    fn commit(&mut self, path: &Path, held: &store::HeldLock<'_>) -> Result<(), WorkloadError> {
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
    validate_scopes(&record.scopes)?;
    Ok(WorkloadCredential {
        id: record.id.clone(),
        public_key,
        scopes: record.scopes.clone(),
        expires_at: record.expires_at,
        revoked_at: record.revoked_at,
    })
}

/// Every scope must parse, there must be at least one, and together they
/// must fit one canonical set. One bad string refuses the whole record:
/// there is no partial grant.
///
/// # Errors
///
/// [`WorkloadError::NoScopes`], the first [`WorkloadError::InvalidScope`],
/// or [`WorkloadError::Malformed`] for more than 64 distinct selectors.
pub fn validate_scopes(scopes: &[String]) -> Result<(), WorkloadError> {
    parse_ceiling(scopes).map(drop)
}

fn parse_ceiling(scopes: &[String]) -> Result<TerminalScopeSet, WorkloadError> {
    if scopes.is_empty() {
        return Err(WorkloadError::NoScopes);
    }
    let ceiling = TerminalScopeSet::parse_all(scopes).map_err(|(index, rule)| {
        rule.map_or_else(
            || {
                WorkloadError::Malformed(
                    "a scope ceiling names more than 64 distinct selectors".to_owned(),
                )
            },
            |reason| WorkloadError::InvalidScope {
                index: index + 1,
                reason,
            },
        )
    })?;
    refuse_restartable_ids(scopes)?;
    // Admission mints exactly this effective set (workload-auth §5.1), so a
    // ceiling it cannot hold is refused here, at enrollment and load, rather
    // than at every HELLO.
    EffectiveScopeSet::unattenuated(&ceiling).map_err(|_| {
        WorkloadError::Malformed("a scope ceiling encodes larger than section 5 allows".to_owned())
    })?;
    Ok(ceiling)
}

/// Refuse `group:` and `terminal:` selectors in a persisted ceiling: the ids
/// they name restart with the server, so a registry record would come to name
/// a different session or Terminal (workload-auth §5.1).
fn refuse_restartable_ids(scopes: &[String]) -> Result<(), WorkloadError> {
    let restartable = |scope: &String| {
        ScopeGrant::parse(scope).is_ok_and(|grant| names_restartable_id(&grant.selector))
    };
    scopes.iter().position(restartable).map_or(Ok(()), |index| {
        Err(WorkloadError::UnstableSelector { index: index + 1 })
    })
}

const fn names_restartable_id(selector: &Selector) -> bool {
    matches!(
        selector,
        Selector::Group(_) | Selector::TerminalLocal(_) | Selector::TerminalSatellite(..)
    )
}

/// An expiry must lie after now and within [`MAX_EXPIRY_SECONDS`].
///
/// # Errors
///
/// [`WorkloadError::InvalidExpiry`].
pub fn validate_expiry(expires_at: i64) -> Result<(), WorkloadError> {
    let now = Utc::now().timestamp();
    if expires_at > now && expires_at - now <= MAX_EXPIRY_SECONDS {
        Ok(())
    } else {
        Err(WorkloadError::InvalidExpiry)
    }
}

/// Canonical workload credential id for a raw public key.
#[must_use]
pub fn credential_id(public_key: &[u8]) -> String {
    format!("{DIGEST_PREFIX}{}", hex::encode(Sha256::digest(public_key)))
}

/// Whether `id` is `sha256:` followed by exactly 64 lowercase hex digits.
#[must_use]
pub fn is_canonical_credential_id(id: &str) -> bool {
    id.strip_prefix(DIGEST_PREFIX).is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Overwrite a buffer that held key material before it is dropped. Best
/// effort: an allocator or a crypto library may already hold another copy.
pub fn scrub(buffer: &mut [u8]) {
    buffer.fill(0);
    std::hint::black_box(buffer);
}

/// Default persisted CA certificate path.
#[must_use]
pub fn default_ca_cert_path() -> PathBuf {
    crate::telemetry::state_dir().join("workload-ca.pem")
}

/// Default persisted CA private-key path.
#[must_use]
pub fn default_ca_key_path() -> PathBuf {
    crate::telemetry::state_dir().join("workload-ca.key")
}

/// Default persisted workload-key registry path.
#[must_use]
pub fn default_registry_path() -> PathBuf {
    crate::telemetry::state_dir().join("workload-keys")
}

/// The workload authority after [`init_authority`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityStatus {
    /// `sha256:` fingerprint of the DER CA certificate.
    pub fingerprint: String,
    /// Whether this call minted the CA.
    pub created: bool,
}

/// Create the persisted workload CA if neither half exists, under the lock of
/// the key's directory, and report its fingerprint. An existing pair is never
/// replaced; a partial pair is an error.
///
/// # Errors
///
/// A partial or insecure pair, or any failure to mint or persist it.
pub fn init_authority(cert_path: &Path, key_path: &Path) -> Result<AuthorityStatus, WorkloadError> {
    let created = store::with_lock(key_path, |held| {
        create_authority_if_absent(cert_path, key_path, held)
    })?;
    Ok(AuthorityStatus {
        fingerprint: ca_fingerprint(cert_path)?,
        created,
    })
}

/// Provision a stable self-signed workload CA if neither file exists.
///
/// # Errors
///
/// As [`init_authority`].
pub fn ensure_ca(cert_path: &Path, key_path: &Path) -> Result<(), WorkloadError> {
    init_authority(cert_path, key_path).map(drop)
}

fn create_authority_if_absent(
    cert_path: &Path,
    key_path: &Path,
    held: &store::HeldLock<'_>,
) -> Result<bool, WorkloadError> {
    let cert = store::probe(cert_path, FileRole::CaCertificate)?.is_some();
    let key = store::probe(key_path, FileRole::CaPrivateKey)?.is_some();
    match (cert, key) {
        (true, true) => Ok(false),
        (true, false) => Err(WorkloadError::PartialPair {
            present: "certificate",
            missing: "private key",
        }),
        (false, true) => Err(WorkloadError::PartialPair {
            present: "private key",
            missing: "certificate",
        }),
        (false, false) => mint_authority(cert_path, key_path, held).map(|()| true),
    }
}

/// Mint the CA pair under the lock on the key's directory (`held`). The
/// certificate may live in another directory (`PHUX_WORKLOAD_CA`); that write
/// sweeps no temporaries there, because that directory is not locked.
fn mint_authority(
    cert_path: &Path,
    key_path: &Path,
    held: &store::HeldLock<'_>,
) -> Result<(), WorkloadError> {
    let mut params = CertificateParams::new(vec!["phux-workload-ca".to_owned()])?;
    params
        .distinguished_name
        .push(DnType::CommonName, "phux workload authority");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let key = KeyPair::generate()?;
    let certificate = params.self_signed(&key)?;
    store::open_owner_dir(cert_path)?;
    // The key lands first: a crash before the certificate leaves a partial
    // pair, which refuses to start rather than minting a second authority.
    let mut key_pem = key.serialize_pem().into_bytes();
    let written = store::atomic_replace(key_path, &key_pem, held);
    scrub(&mut key_pem);
    written?;
    store::atomic_replace(cert_path, certificate.pem().as_bytes(), held)
}

/// SHA-256 CA fingerprint in the canonical `sha256:` form.
///
/// # Errors
///
/// [`WorkloadError::AuthorityMissing`] when there is no CA certificate, or a
/// failure to read or parse it.
pub fn ca_fingerprint(cert_path: &Path) -> Result<String, WorkloadError> {
    let (certificate, _) = load_authority_certificate(cert_path)?;
    Ok(credential_id(certificate.as_ref()))
}

/// The CA certificate's DER, read through the store's owner, mode,
/// no-follow, and stable-read checks. TLS verifiers are built from these
/// bytes rather than by re-reading the path.
///
/// # Errors
///
/// [`WorkloadError::AuthorityMissing`] when there is no CA certificate, or a
/// failure to read or parse it.
pub fn authority_certificate(cert_path: &Path) -> Result<CertificateDer<'static>, WorkloadError> {
    load_authority_certificate(cert_path).map(|(certificate, _)| certificate)
}

fn load_authority_certificate(
    cert_path: &Path,
) -> Result<(CertificateDer<'static>, String), WorkloadError> {
    let Some((_, raw)) = store::read_stable(cert_path, FileRole::CaCertificate)? else {
        return Err(WorkloadError::AuthorityMissing);
    };
    let pem = String::from_utf8(raw)
        .map_err(|_| WorkloadError::MalformedAuthority("the CA certificate is not PEM text"))?;
    let certificate = CertificateDer::pem_slice_iter(pem.as_bytes())
        .next()
        .and_then(Result::ok)
        .ok_or(WorkloadError::MalformedAuthority(
            "the CA certificate file holds no certificate",
        ))?;
    Ok((certificate, pem))
}

/// Read and parse the CA private key; the PEM buffer is scrubbed before this
/// returns. rcgen decodes the key into buffers of its own, which this crate
/// cannot reach, so scrubbing is best effort.
fn load_authority_key(key_path: &Path) -> Result<KeyPair, WorkloadError> {
    let Some((_, mut raw)) = store::read_stable(key_path, FileRole::CaPrivateKey)? else {
        return Err(WorkloadError::AuthorityMissing);
    };
    let key = std::str::from_utf8(&raw)
        .ok()
        .and_then(|pem| KeyPair::from_pem(pem).ok());
    scrub(&mut raw);
    key.ok_or(WorkloadError::MalformedAuthority(
        "the CA private key is not a PEM private key",
    ))
}

/// Public enrollment material that has been verified or signed and is ready
/// to commit to the registry.
#[derive(Debug, Clone)]
pub struct PreparedEnrollment {
    public_key: Vec<u8>,
    issued_chain_pem: Option<String>,
}

impl PreparedEnrollment {
    /// The credential id the commit will record.
    #[must_use]
    pub fn credential_id(&self) -> String {
        credential_id(&self.public_key)
    }

    /// For a signed CSR, the issued certificate followed by the CA
    /// certificate, in PEM. Public material; `None` for a supplied
    /// certificate.
    #[must_use]
    pub fn issued_chain_pem(&self) -> Option<&str> {
        self.issued_chain_pem.as_deref()
    }

    /// Record the credential in the registry at `registry`.
    ///
    /// # Errors
    ///
    /// As [`WorkloadRegistry::register`].
    pub fn commit(
        &self,
        registry: &Path,
        scopes: Vec<String>,
        expires_at: i64,
    ) -> Result<RegisteredCredential, WorkloadError> {
        WorkloadRegistry::register(registry, &self.public_key, scopes, Some(expires_at))
    }
}

/// Verify or sign `material` against the workload authority at `paths`.
///
/// The registry is not touched. A certificate must chain to the CA; a CSR's
/// self-signature must verify, and the CA then issues a client certificate
/// valid until `expires_at` for the requester's public key.
///
/// # Errors
///
/// [`WorkloadError::InvalidExpiry`], [`WorkloadError::AuthorityMissing`],
/// [`WorkloadError::NotIssuedByAuthority`], a [`MaterialError`], or a failure
/// reading the CA.
pub fn prepare_enrollment(
    paths: &WorkloadPaths,
    material: &ClientMaterial,
    expires_at: i64,
) -> Result<PreparedEnrollment, WorkloadError> {
    validate_expiry(expires_at)?;
    let (ca_certificate, ca_pem) = load_authority_certificate(&paths.ca_cert)?;
    match material {
        ClientMaterial::Certificate {
            leaf,
            intermediates,
        } => {
            verify_issued_by_authority(&ca_certificate, leaf, intermediates)?;
            Ok(PreparedEnrollment {
                public_key: subject_public_key_info(leaf.as_ref())?,
                issued_chain_pem: None,
            })
        }
        ClientMaterial::Request(request) => {
            let issued = sign_request(&paths.ca_key, &ca_certificate, request, expires_at)?;
            Ok(PreparedEnrollment {
                public_key: subject_public_key_info(issued.der().as_ref())?,
                issued_chain_pem: Some(format!("{}{ca_pem}", issued.pem())),
            })
        }
    }
}

/// The same verifier the TLS acceptor uses, so a certificate accepted here
/// is one the handshake accepts.
fn verify_issued_by_authority(
    ca_certificate: &CertificateDer<'static>,
    leaf: &CertificateDer<'static>,
    intermediates: &[CertificateDer<'static>],
) -> Result<(), WorkloadError> {
    let verifier = crate::transport::tls::client_verifier(ca_certificate).map_err(|_| {
        WorkloadError::MalformedAuthority("the CA certificate cannot verify client certificates")
    })?;
    verifier
        .verify_client_cert(leaf, intermediates, UnixTime::now())
        .map(drop)
        .map_err(|_| WorkloadError::NotIssuedByAuthority)
}

fn sign_request(
    ca_key_path: &Path,
    ca_certificate: &CertificateDer<'static>,
    request: &CertificateSigningRequestDer<'static>,
    expires_at: i64,
) -> Result<rcgen::Certificate, WorkloadError> {
    // `from_der` verifies the request's self-signature: proof the requester
    // holds the private key it asks to have certified.
    let requested = CertificateSigningRequestParams::from_der(request)
        .map_err(|_| MaterialError::InvalidRequest)?;
    let issuer = Issuer::from_ca_cert_der(ca_certificate, load_authority_key(ca_key_path)?)?;
    // Only the public key is taken from the request; every other field is
    // this authority's choice, so a request cannot ask to become a CA.
    let certified = CertificateSigningRequestParams {
        params: client_certificate_params(expires_at)?,
        public_key: requested.public_key,
    };
    Ok(certified.signed_by(&issuer)?)
}

fn client_certificate_params(expires_at: i64) -> Result<CertificateParams, WorkloadError> {
    let expiry = DateTime::from_timestamp(expires_at, 0).ok_or(WorkloadError::InvalidExpiry)?;
    let mut params = CertificateParams::new(vec!["phux-workload-client".to_owned()])?;
    params
        .distinguished_name
        .push(DnType::CommonName, "phux workload client");
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.use_authority_key_identifier_extension = true;
    // Day granularity with a day of slack on each side: the registry expiry,
    // not the certificate, is the authoritative policy time.
    let (year, month, day) = calendar_day(Utc::now() - chrono::Duration::days(1));
    params.not_before = rcgen::date_time_ymd(year, month, day);
    let (year, month, day) = calendar_day(expiry + chrono::Duration::days(1));
    params.not_after = rcgen::date_time_ymd(year, month, day);
    Ok(params)
}

/// `(year, month, day)` in UTC. chrono's month (1..=12) and day (1..=31)
/// always fit a `u8`, and [`MAX_EXPIRY_SECONDS`] keeps the year in range.
fn calendar_day(at: DateTime<Utc>) -> (i32, u8, u8) {
    let narrow = |value: u32| u8::try_from(value).unwrap_or(u8::MAX);
    (at.year(), narrow(at.month()), narrow(at.day()))
}

fn subject_public_key_info(certificate: &[u8]) -> Result<Vec<u8>, WorkloadError> {
    let (_, parsed) = x509_parser::parse_x509_certificate(certificate)
        .map_err(|_| MaterialError::InvalidCertificate)?;
    Ok(parsed.tbs_certificate.subject_pki.raw.to_vec())
}

/// Enroll a client certificate signed by the persisted workload CA.
///
/// The client key is generated locally and written owner-only. The returned
/// id is also inserted into `registry_path`; callers deliver the certificate
/// and key through their pairing channel, never through the protocol stream.
///
/// # Errors
///
/// Any failure reading the CA, minting the client pair, or registering it.
pub fn enroll_client(
    ca_cert_path: &Path,
    ca_key_path: &Path,
    cert_path: &Path,
    key_path: &Path,
    registry_path: &Path,
    scopes: Vec<String>,
) -> Result<String, WorkloadError> {
    let (ca_certificate, ca_pem) = load_authority_certificate(ca_cert_path)?;
    let issuer = Issuer::from_ca_cert_der(&ca_certificate, load_authority_key(ca_key_path)?)?;
    let client_key = KeyPair::generate()?;
    let public_key = client_key.subject_public_key_info();
    let mut params = CertificateParams::new(vec!["phux-workload-client".to_owned()])?;
    params
        .distinguished_name
        .push(DnType::CommonName, "phux workload client");
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let certificate = params.signed_by(&client_key, &issuer)?;
    for parent in [cert_path.parent(), key_path.parent()]
        .into_iter()
        .flatten()
    {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let mut key_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(key_path)?;
    let mut key_pem = client_key.serialize_pem().into_bytes();
    let written = key_file.write_all(&key_pem);
    scrub(&mut key_pem);
    written?;
    let id = WorkloadRegistry::register(registry_path, &public_key, scopes, None)?.id;
    // Keep the CA in the client chain so a TLS peer can build the path even
    // when its trust store contains only the client certificate bundle.
    fs::write(cert_path, format!("{}{ca_pem}", certificate.pem()))?;
    Ok(id)
}

/// Lowercase hex only: the registry has one spelling per key.
fn decode_lower_hex(value: &str) -> Option<Vec<u8>> {
    let lower = value
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    lower.then(|| hex::decode(value).ok()).flatten()
}

#[cfg(test)]
#[path = "workload/tests.rs"]
mod tests;
