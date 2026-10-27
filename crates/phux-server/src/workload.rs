//! mTLS workload authority material and registry (ADR-0116).
//!
//! TLS proves key possession; this module decides which public keys are
//! admitted and with what scope ceiling. The CA is separate from the server
//! leaf, so leaves renew without changing workload identity. Files are
//! owner-only, no-follow, and replaced atomically under a lock
//! (`docs/remote.md#who-can-connect`); each write advances a generation a running
//! server observes ([`ReloadingWorkloadRegistry`]). Enrollment accepts only
//! public material ([`ClientMaterial`]), and no diagnostic names the CA key
//! path.

mod authority;
mod client;
mod enrollment;
mod material;
mod registry;
mod reload;
mod server_identity;
mod store;
pub mod tickets;
mod validate;

pub use authority::*;
pub use enrollment::*;
pub use registry::*;
pub use validate::*;

pub use client::{
    ClientRequest, IssuedIdentity, remove_identity_files, remove_owned_file,
    stored_certificate_expiry, stored_chain_authority, stored_credential_id,
};

pub use material::{ClientMaterial, MAX_MATERIAL_BYTES, MaterialError};
pub use phux_protocol::scope::ScopeGrammarError;
use phux_protocol::scope::TerminalScopeSet;
pub use reload::{BrokenRegistry, RegistryObservation, ReloadingWorkloadRegistry};
pub use server_identity::{
    IssuedServerIdentity, Rotation, ServerPair, issue_server_identity, rotate_authority,
};

use std::io;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// Names `tests` reaches through `use super::*`.
#[cfg(test)]
use rcgen::{
    BasicConstraints, CertificateParams, CertificateSigningRequestParams, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
#[cfg(test)]
use rustls::pki_types::pem::PemObject;
#[cfg(test)]
use rustls::pki_types::{CertificateDer, CertificateSigningRequestDer, UnixTime};
#[cfg(test)]
use std::path::Path;

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
    /// (`terminal:`) id, which restarts with the server; the keyed
    /// `session:<key>` and `resource:<key>` name the same records durably.
    /// `index` is 1-based.
    #[error(
        "workload scope {index}: group: and terminal: selectors name ids that restart with the server, so a registry cannot hold them; name the record with session:<key> or resource:<key>"
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
    /// A development build aimed at the production workload authority
    /// ([`phux_config::production::refuse_dev_on_production_state`]).
    #[error("{0}")]
    ProductionState(String),
    /// An authority's reply to a client CSR was refused
    /// ([`ClientRequest::accept`]); the reply is never echoed.
    #[error("the issued client certificate was refused: {0}")]
    IssuedMismatch(&'static str),
    /// A client identity file already exists where a new one would go.
    #[error("a client identity file already exists; a new identity is never written over one")]
    AlreadyStored,
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
    /// default under the state directory. For the CLI verbs; the server
    /// runtime resolves them through its [`crate::runtime::ServerEnv`].
    #[must_use]
    pub fn from_env() -> Self {
        let path = |var: &str| std::env::var_os(var).map(PathBuf::from);
        Self::with_overrides(
            path("PHUX_WORKLOAD_CA"),
            path("PHUX_WORKLOAD_CA_KEY"),
            path("PHUX_WORKLOAD_KEYS"),
        )
    }

    /// The given locations, each defaulting under the state directory.
    #[must_use]
    pub fn with_overrides(
        ca_cert: Option<PathBuf>,
        ca_key: Option<PathBuf>,
        registry: Option<PathBuf>,
    ) -> Self {
        Self {
            ca_cert: ca_cert.unwrap_or_else(default_ca_cert_path),
            ca_key: ca_key.unwrap_or_else(default_ca_key_path),
            registry: registry.unwrap_or_else(default_registry_path),
        }
    }
}

/// The ceiling a paired device enrolls with: every verb on the whole host.
///
/// `phux host add` enrolls it over ssh and a `phux pair --enroll` ticket
/// carries it. Enrollment grants no authority ssh did not already grant. Spelled out
/// rather than `*`, which a far host's shell would glob.
pub const PAIRED_DEVICE_SCOPE: &str =
    "inventory,observe,create,bind,input,signal,organize,terminate@global";

/// The original pairing ceiling, retained only for legacy principal
/// inference and an explicit owner-requested repair. Loading it never adds verbs.
pub(crate) const LEGACY_PAIRED_DEVICE_SCOPE: &str =
    "inventory,observe,create,bind,input,signal@global";

/// Who a registry credential was enrolled for (ADR-0163 §6,
/// `docs/remote.md#who-can-connect`): a person's paired device, or an automation.
/// It decides input-lease precedence, never scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrolledFor {
    /// A device a person paired (phone, desktop): a human principal.
    Human,
    /// An unattended workload: a non-human principal.
    Automation,
}

impl EnrolledFor {
    /// The spelling the registry and `phux workload` use.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Automation => "automation",
        }
    }

    /// Who a credential recorded without the field was enrolled for, by its
    /// enrollment origin. Before pairing recorded the principal explicitly,
    /// it used exactly the legacy six-verb pairing ceiling. Do not compare
    /// against today's full ceiling: that would reclassify old wildcard
    /// automation as human when the verb vocabulary grows.
    #[must_use]
    pub fn by_ceiling(scopes: &[String]) -> Self {
        let paired = parse_ceiling(&[LEGACY_PAIRED_DEVICE_SCOPE.to_owned()]).ok();
        if paired.is_some() && parse_ceiling(scopes).ok() == paired {
            Self::Human
        } else {
            Self::Automation
        }
    }

    /// The principal kind a connection admitted under it acts as.
    #[must_use]
    pub const fn kind(self) -> phux_core::actor::PrincipalKind {
        match self {
            Self::Human => phux_core::actor::PrincipalKind::Human,
            Self::Automation => phux_core::actor::PrincipalKind::Automation,
        }
    }
}

impl std::str::FromStr for EnrolledFor {
    type Err = WorkloadError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "human" => Ok(Self::Human),
            "automation" => Ok(Self::Automation),
            _ => Err(WorkloadError::Malformed(format!(
                "principal must be human or automation, not {value:?}"
            ))),
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
    /// Who it was enrolled for, as recorded; `None` for a record that
    /// predates the field ([`Self::enrolled_for`] infers it).
    pub recorded_for: Option<EnrolledFor>,
    /// The satellite this credential is (`docs/remote.md#linking-machines`,
    /// ADR-0182): admitted only on the satellite ALPN, never on the terminal
    /// endpoint, and it carries no scope ceiling. `None` for every other
    /// credential.
    pub satellite: Option<String>,
}

impl WorkloadCredential {
    /// Who it was enrolled for: as recorded, else
    /// [`EnrolledFor::by_ceiling`].
    #[must_use]
    pub fn enrolled_for(&self) -> EnrolledFor {
        self.recorded_for
            .unwrap_or_else(|| EnrolledFor::by_ceiling(&self.scopes))
    }

    /// Neither revoked nor expired at Unix time `now`.
    #[must_use]
    pub fn is_active_at(&self, now: i64) -> bool {
        self.revoked_at.is_none() && self.expires_at.is_none_or(|expires| expires > now)
    }

    /// The scope ceiling as one canonical set (`docs/remote.md#who-can-do-what`).
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

#[cfg(test)]
#[path = "workload/tests.rs"]
mod tests;
