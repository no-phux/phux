//! mTLS workload authority material and registry (ADR-0116).
//!
//! TLS proves possession of a client private key; this module owns the local
//! authority that decides which public keys are admitted and what their scope
//! ceiling is.  The certificate authority is deliberately separate from the
//! server leaf certificate: clients pin the CA fingerprint, while leaf
//! certificates may be renewed without changing workload identity.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use chrono::Utc;
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, Issuer, KeyPair, PublicKeyData};
use rustls::pki_types::pem::PemObject;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const STORE_VERSION: u32 = 1;

/// Errors from provisioning or loading workload authority material.
#[derive(Debug, thiserror::Error)]
pub enum WorkloadError {
    /// Filesystem operation failed.
    #[error("workload authority io: {0}")]
    Io(#[from] io::Error),
    /// Certificate generation failed.
    #[error("workload authority certificate: {0}")]
    Certificate(#[from] rcgen::Error),
    /// A persisted registry is malformed or violates its invariants.
    #[error("malformed workload registry: {0}")]
    Malformed(String),
    /// One half of a persistent pair exists.
    #[error("partial workload authority pair: {present} exists but {missing} is missing")]
    PartialPair {
        /// The path that survived.
        present: String,
        /// The path that is absent.
        missing: String,
    },
}

/// A workload credential's canonical scope ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkloadCredential {
    /// `sha256:` identifier derived from the raw public key.
    pub id: String,
    /// Raw public key, represented as lowercase hex in the registry.
    pub public_key: Vec<u8>,
    /// Closed scope names granted to this credential.
    pub scopes: Vec<String>,
    /// Absolute Unix expiry, if any.
    pub expires_at: Option<i64>,
    /// Revocation time, if revoked.
    pub revoked_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    version: u32,
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
}

impl WorkloadRegistry {
    /// Load a registry. A missing file is an empty authority snapshot.
    pub fn load(path: &Path) -> Result<Self, WorkloadError> {
        let raw = match fs::read(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::empty()),
            Err(error) => return Err(error.into()),
        };
        let file: RegistryFile = serde_json::from_slice(&raw)
            .map_err(|error| WorkloadError::Malformed(error.to_string()))?;
        if file.version != STORE_VERSION {
            return Err(WorkloadError::Malformed(format!(
                "unsupported version {}",
                file.version
            )));
        }
        let mut credentials = Vec::with_capacity(file.credentials.len());
        for record in file.credentials {
            let public_key = decode_hex(&record.public_key)
                .ok_or_else(|| WorkloadError::Malformed("public_key is not hex".to_owned()))?;
            if public_key.is_empty() {
                return Err(WorkloadError::Malformed("public_key is empty".to_owned()));
            }
            let expected = credential_id(&public_key);
            if record.id != expected {
                return Err(WorkloadError::Malformed(format!(
                    "credential id does not match public_key: {}",
                    record.id
                )));
            }
            if record.scopes.iter().any(String::is_empty) {
                return Err(WorkloadError::Malformed(
                    "scope names must not be empty".to_owned(),
                ));
            }
            credentials.push(WorkloadCredential {
                id: record.id,
                public_key,
                scopes: record.scopes,
                expires_at: record.expires_at,
                revoked_at: record.revoked_at,
            });
        }
        Ok(Self { credentials })
    }

    /// An empty authority snapshot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            credentials: Vec::new(),
        }
    }

    /// Find an active credential by its public-key-derived id.
    #[must_use]
    pub fn lookup(&self, id: &str) -> Option<&WorkloadCredential> {
        let now = Utc::now().timestamp();
        self.credentials.iter().find(|credential| {
            credential.id == id
                && credential.revoked_at.is_none()
                && credential.expires_at.is_none_or(|expires| expires > now)
        })
    }

    /// Find an active credential from a TLS leaf certificate's DER bytes.
    /// rustls has already verified the chain; this method only extracts the
    /// stable `SubjectPublicKeyInfo` used as the registry identity.
    #[must_use]
    pub fn lookup_certificate(&self, certificate: &[u8]) -> Option<&WorkloadCredential> {
        let (_, parsed) = x509_parser::parse_x509_certificate(certificate).ok()?;
        let id = credential_id(parsed.tbs_certificate.subject_pki.raw);
        self.lookup(&id)
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
    /// updated snapshot. The returned id is stable across certificate renewal.
    pub fn register(
        path: &Path,
        public_key: &[u8],
        scopes: Vec<String>,
        expires_at: Option<i64>,
    ) -> Result<String, WorkloadError> {
        if public_key.is_empty() || scopes.iter().any(String::is_empty) {
            return Err(WorkloadError::Malformed(
                "public key and scopes must be non-empty".to_owned(),
            ));
        }
        let id = credential_id(public_key);
        let mut registry = Self::load(path)?;
        if registry
            .credentials
            .iter()
            .any(|credential| credential.id == id)
        {
            return Err(WorkloadError::Malformed(format!(
                "credential {id} is already registered"
            )));
        }
        registry.credentials.push(WorkloadCredential {
            id: id.clone(),
            public_key: public_key.to_vec(),
            scopes,
            expires_at,
            revoked_at: None,
        });
        let records = registry
            .credentials
            .iter()
            .map(|credential| RegistryRecord {
                id: credential.id.clone(),
                public_key: encode_hex(&credential.public_key),
                scopes: credential.scopes.clone(),
                expires_at: credential.expires_at,
                revoked_at: credential.revoked_at,
            })
            .collect();
        let bytes = serde_json::to_vec_pretty(&RegistryFile {
            version: STORE_VERSION,
            credentials: records,
        })
        .map_err(|error| WorkloadError::Malformed(error.to_string()))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(&bytes)?;
        Ok(id)
    }
}

/// Canonical workload credential id for a raw public key.
#[must_use]
pub fn credential_id(public_key: &[u8]) -> String {
    let digest = Sha256::digest(public_key);
    let mut id = String::from("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(id, "{byte:02x}");
    }
    id
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

/// Provision a stable self-signed workload CA if neither file exists.
pub fn ensure_ca(cert_path: &Path, key_path: &Path) -> Result<(), WorkloadError> {
    match (cert_path.exists(), key_path.exists()) {
        (true, true) => return Ok(()),
        (true, false) => {
            return Err(WorkloadError::PartialPair {
                present: cert_path.display().to_string(),
                missing: key_path.display().to_string(),
            });
        }
        (false, true) => {
            return Err(WorkloadError::PartialPair {
                present: key_path.display().to_string(),
                missing: cert_path.display().to_string(),
            });
        }
        (false, false) => {}
    }

    let mut params = CertificateParams::new(vec!["phux-workload-ca".to_owned()])?;
    params
        .distinguished_name
        .push(DnType::CommonName, "phux workload authority");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let key = KeyPair::generate()?;
    let certificate = params.self_signed(&key)?;
    if let Some(parent) = cert_path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    fs::write(cert_path, certificate.pem())?;
    let mut key_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(key_path)?;
    key_file.write_all(key.serialize_pem().as_bytes())?;
    Ok(())
}

/// Enroll a client certificate signed by the persisted workload CA.
///
/// The client key is generated locally and written owner-only. The returned
/// id is also inserted into `registry_path`; callers deliver the certificate
/// and key through their pairing channel, never through the protocol stream.
pub fn enroll_client(
    ca_cert_path: &Path,
    ca_key_path: &Path,
    cert_path: &Path,
    key_path: &Path,
    registry_path: &Path,
    scopes: Vec<String>,
) -> Result<String, WorkloadError> {
    let ca_key = KeyPair::from_pem(&fs::read_to_string(ca_key_path)?)?;
    let ca_cert = rustls::pki_types::CertificateDer::pem_file_iter(ca_cert_path)
        .map_err(|error| WorkloadError::Malformed(error.to_string()))?
        .next()
        .ok_or_else(|| WorkloadError::Malformed("CA certificate is empty".to_owned()))?
        .map_err(|error| WorkloadError::Malformed(error.to_string()))?;
    let client_key = KeyPair::generate()?;
    let public_key = client_key.subject_public_key_info();
    let mut params = CertificateParams::new(vec!["phux-workload-client".to_owned()])?;
    params
        .distinguished_name
        .push(DnType::CommonName, "phux workload client");
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    let mut issuer_params = CertificateParams::new(vec!["phux-workload-ca".to_owned()])?;
    issuer_params
        .distinguished_name
        .push(DnType::CommonName, "phux workload authority");
    let issuer = Issuer::from_params(&issuer_params, ca_key);
    let certificate = params.signed_by(&client_key, &issuer)?;
    if let Some(parent) = cert_path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    fs::write(cert_path, certificate.pem())?;
    let mut key_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(key_path)?;
    key_file.write_all(client_key.serialize_pem().as_bytes())?;
    let id = WorkloadRegistry::register(registry_path, &public_key, scopes, None)?;
    // Keep the CA in the client chain so a TLS peer can build the path even
    // when its trust store contains only the client certificate bundle.
    let mut chain = fs::read_to_string(cert_path)?;
    chain.push_str(&fs::read_to_string(ca_cert_path)?);
    fs::write(cert_path, chain)?;
    let _ = ca_cert;
    Ok(id)
}

/// SHA-256 CA fingerprint in the canonical `sha256:` form.
pub fn ca_fingerprint(cert_path: &Path) -> Result<String, WorkloadError> {
    let certs = rustls::pki_types::CertificateDer::pem_file_iter(cert_path)
        .map_err(|error| WorkloadError::Malformed(error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| WorkloadError::Malformed(error.to_string()))?;
    let leaf = certs
        .first()
        .ok_or_else(|| WorkloadError::Malformed("CA certificate is empty".to_owned()))?;
    Ok(credential_id(leaf.as_ref()))
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).ok())
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn ca_provisioning_is_stable_and_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("ca.pem");
        let key = dir.path().join("ca.key");
        ensure_ca(&cert, &key).unwrap();
        let first = ca_fingerprint(&cert).unwrap();
        ensure_ca(&cert, &key).unwrap();
        assert_eq!(ca_fingerprint(&cert).unwrap(), first);
        assert_eq!(
            std::fs::metadata(key).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn credential_ids_are_canonical() {
        let id = credential_id(b"public key");
        assert!(id.starts_with("sha256:"));
        assert_eq!(id.len(), 71);
        assert!(
            id[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
    }

    #[test]
    fn enrollment_writes_a_client_chain_and_registry_record() {
        let dir = tempfile::tempdir().unwrap();
        let ca_cert = dir.path().join("ca.pem");
        let ca_key = dir.path().join("ca.key");
        let client_cert = dir.path().join("client.pem");
        let client_key = dir.path().join("client.key");
        let registry = dir.path().join("workload-keys");
        ensure_ca(&ca_cert, &ca_key).unwrap();
        let id = enroll_client(
            &ca_cert,
            &ca_key,
            &client_cert,
            &client_key,
            &registry,
            vec!["terminal.control".to_owned()],
        )
        .unwrap();
        assert!(id.starts_with("sha256:"));
        assert_eq!(
            WorkloadRegistry::load(&registry)
                .unwrap()
                .lookup(&id)
                .unwrap()
                .scopes,
            ["terminal.control"]
        );
        assert!(
            std::fs::read_to_string(&client_cert)
                .unwrap()
                .matches("BEGIN CERTIFICATE")
                .count()
                >= 2
        );
    }
}
