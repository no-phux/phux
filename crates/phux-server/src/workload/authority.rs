//! The workload CA: provisioning, fingerprints, and loading its material.

use std::path::Path;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;

use super::store::{self, FileRole};
use super::{WorkloadError, credential_id, scrub};

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
    let params = authority_params()?;
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

/// The fields of every workload CA certificate.
pub(super) fn authority_params() -> Result<CertificateParams, WorkloadError> {
    let mut params = CertificateParams::new(vec!["phux-workload-ca".to_owned()])?;
    params
        .distinguished_name
        .push(DnType::CommonName, "phux workload authority");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    Ok(params)
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

pub(super) fn load_authority_certificate(
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
pub(super) fn load_authority_key(key_path: &Path) -> Result<KeyPair, WorkloadError> {
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
