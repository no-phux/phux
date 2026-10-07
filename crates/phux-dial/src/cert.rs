//! Self-signed certificate provisioning for phux TLS listeners.
//!
//! The provisioning half of [`crate::tls`]'s pinning, shared by `phux-server`
//! and `phux-relay` (which ADR-0051 forbids from depending on the server).
//!
//! Contract:
//!
//! - **A complete pair is never touched.** Its fingerprint is pinned on
//!   devices phux cannot reach; regenerating would silently un-pair them.
//! - **A half-present pair is refused**, not repaired, for the same reason.
//! - **SANs are chosen once, at generation** (ADR-0091); an existing
//!   certificate is never widened.
//! - **The key is owner-only (`0o600`); the certificate is public.**

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};

/// Errors from provisioning or reading persisted TLS material. Callers map
/// this into their own error vocabulary.
#[derive(Debug, thiserror::Error)]
pub enum CertError {
    /// A certificate or key file could not be read or written.
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// Generating the self-signed certificate failed.
    #[error("certificate generation: {0}")]
    Rcgen(#[from] rcgen::Error),
    /// A PEM certificate or key file could not be parsed.
    #[error("pem: {0}")]
    Pem(#[from] rustls::pki_types::pem::Error),
    /// The certificate file held no certificates. Carries the path.
    #[error("no certificates in {0}")]
    NoCerts(String),
    /// The private key file is owned by another account, or other accounts
    /// can read or replace it ([`crate::secret_file`]). Carries the refusal.
    #[error("{0}")]
    InsecureKey(String),
    /// Exactly one of the persisted cert/key pair exists; the operator must
    /// delete the survivor explicitly.
    #[error("partial TLS pair: {present} exists but {missing} is missing")]
    PartialTlsPair {
        /// Path of the file that still exists.
        present: String,
        /// Path of the file that is missing.
        missing: String,
    },
}

/// SANs every generated certificate carries: the loopback identities.
pub const LOOPBACK_SANS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// [`ensure_self_signed_for`] naming only the loopback identities.
///
/// # Errors
///
/// As [`ensure_self_signed_for`].
pub fn ensure_self_signed(cert_path: &Path, key_path: &Path) -> Result<(), CertError> {
    ensure_self_signed_for(cert_path, key_path, &[])
}

/// Provision a self-signed certificate + key when both files are missing.
///
/// SANs name the loopback identities, then `advertised` (IP literals or DNS
/// names). A complete pair is left untouched and never widened (ADR-0091).
///
/// # Errors
///
/// [`CertError::PartialTlsPair`] when exactly one of the two files exists;
/// [`CertError::Rcgen`] if generation fails; [`CertError::Io`] if a parent
/// directory or either file cannot be written.
pub fn ensure_self_signed_for(
    cert_path: &Path,
    key_path: &Path,
    advertised: &[String],
) -> Result<(), CertError> {
    let partial = |present: &Path, missing: &Path| CertError::PartialTlsPair {
        present: present.display().to_string(),
        missing: missing.display().to_string(),
    };
    match (cert_path.exists(), key_path.exists()) {
        (true, true) => return Ok(()),
        (false, false) => {}
        (true, false) => return Err(partial(cert_path, key_path)),
        (false, true) => return Err(partial(key_path, cert_path)),
    }
    let certified = rcgen::generate_simple_self_signed(san_list(advertised))?;
    write_pair(
        cert_path,
        key_path,
        &certified.cert.pem(),
        &certified.signing_key.serialize_pem(),
    )
}

/// Write a freshly provisioned pair, creating missing parent directories.
///
/// The PEM certificate chain is public; the PEM private key is written
/// owner-only (`0o600`). The callers write only where both files were absent.
///
/// # Errors
///
/// [`CertError::Io`] if a directory or either file cannot be written.
pub fn write_pair(
    cert_path: &Path,
    key_path: &Path,
    cert_pem: &str,
    key_pem: &str,
) -> Result<(), CertError> {
    if let Some(parent) = cert_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(cert_path, cert_pem)?;
    let mut key_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(key_path)?;
    io::Write::write_all(&mut key_file, key_pem.as_bytes())?;
    Ok(())
}

/// The SAN list for a fresh certificate: [`LOOPBACK_SANS`] then the trimmed
/// advertised names, de-duplicated in order. Empty entries are dropped (rcgen
/// rejects an empty DNS name).
#[must_use]
pub fn san_list(advertised: &[String]) -> Vec<String> {
    let mut sans: Vec<String> = LOOPBACK_SANS.iter().map(|s| (*s).to_owned()).collect();
    for name in advertised {
        let name = name.trim();
        if !name.is_empty() && !sans.iter().any(|existing| existing == name) {
            sans.push(name.to_owned());
        }
    }
    sans
}

/// SHA-256 fingerprint of the leaf certificate as uppercase colon-separated
/// hex (`AB:CD:…`): the shape `phux pair` prints and [`crate::CertTrust`] pins.
///
/// # Errors
///
/// [`CertError::Pem`] if the file cannot be parsed, [`CertError::NoCerts`] if
/// it holds no certificate.
pub fn cert_fingerprint(cert_path: &Path) -> Result<String, CertError> {
    let certs = load_certs(cert_path)?;
    let leaf = certs
        .first()
        .ok_or_else(|| CertError::NoCerts(cert_path.display().to_string()))?;
    let digest = Sha256::digest(leaf.as_ref());
    let hex: Vec<String> = digest.iter().map(|b| format!("{b:02X}")).collect();
    Ok(hex.join(":"))
}

/// Read the PEM certificate chain, leaf first.
///
/// # Errors
///
/// [`CertError::Pem`] if the file cannot be read or parsed, and
/// [`CertError::NoCerts`] if the chain is empty.
pub fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, CertError> {
    let certs = CertificateDer::pem_file_iter(path)?.collect::<Result<Vec<_>, _>>()?;
    if certs.is_empty() {
        return Err(CertError::NoCerts(path.display().to_string()));
    }
    Ok(certs)
}

/// Read the first PEM private key (PKCS#8, SEC1, or PKCS#1).
///
/// The file must pass [`crate::secret_file::SecretFile::PrivateKey`] first:
/// a key another account owns or can read is refused, and a group-readable
/// one is logged as a warning and used.
///
/// # Errors
///
/// [`CertError::InsecureKey`] for a refused file, and [`CertError::Pem`] if
/// the file cannot be read or holds no private key.
pub fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>, CertError> {
    use crate::secret_file::{SecretFile, check_metadata, effective_uid};
    // A missing file reports as before, through the read.
    if let Ok(metadata) = fs::metadata(path) {
        let verdict = check_metadata(path, &metadata, SecretFile::PrivateKey, effective_uid());
        if let Some(warning) = verdict.map_err(CertError::InsecureKey)? {
            tracing::warn!("{warning}");
        }
    }
    Ok(PrivateKeyDer::from_pem_file(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Provisioning creates missing directories, writes loadable material
    /// with an owner-only key, and is idempotent so the pin stays stable.
    #[test]
    fn provisioning_writes_a_loadable_owner_only_pair_once() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        let cert = nested.join("cert.pem");
        let key = nested.join("key.pem");
        ensure_self_signed(&cert, &key).unwrap();

        let certs = load_certs(&cert).unwrap();
        assert_eq!(certs.len(), 1, "a self-signed leaf, no intermediates");
        load_key(&key).unwrap();
        let mode = fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "private key must be owner-only");

        let digest = Sha256::digest(certs[0].as_ref());
        let expected: Vec<String> = digest.iter().map(|b| format!("{b:02X}")).collect();
        let first = cert_fingerprint(&cert).unwrap();
        assert_eq!(first, expected.join(":"));

        ensure_self_signed(&cert, &key).unwrap();
        assert_eq!(cert_fingerprint(&cert).unwrap(), first);
    }

    /// Either survivor of a broken pair is refused and left untouched.
    #[test]
    fn a_partial_pair_is_refused_from_either_side() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        ensure_self_signed(&cert, &key).unwrap();
        let fp = cert_fingerprint(&cert).unwrap();

        fs::remove_file(&key).unwrap();
        let err = ensure_self_signed(&cert, &key).unwrap_err();
        let CertError::PartialTlsPair { present, missing } = &err else {
            panic!("expected PartialTlsPair, got {err}");
        };
        assert_eq!(present, &cert.display().to_string());
        assert_eq!(missing, &key.display().to_string());
        assert_eq!(cert_fingerprint(&cert).unwrap(), fp, "cert untouched");

        fs::remove_file(&cert).unwrap();
        fs::write(&key, "not-a-real-key").unwrap();
        let err = ensure_self_signed(&cert, &key).unwrap_err();
        let CertError::PartialTlsPair { present, missing } = &err else {
            panic!("expected PartialTlsPair, got {err}");
        };
        assert_eq!(present, &key.display().to_string());
        assert_eq!(missing, &cert.display().to_string());
    }

    #[test]
    fn san_list_keeps_loopback_first_and_dedupes_advertised() {
        assert_eq!(san_list(&[]), LOOPBACK_SANS.map(str::to_owned).to_vec());
        assert_eq!(
            san_list(&[
                "127.0.0.1".to_owned(),
                "  ".to_owned(),
                "100.64.0.2".to_owned(),
                "mini.tail.ts.net".to_owned(),
                "100.64.0.2".to_owned(),
            ]),
            vec![
                "localhost",
                "127.0.0.1",
                "::1",
                "100.64.0.2",
                "mini.tail.ts.net"
            ]
        );
    }

    /// ADR-0091: the advertised address reaches the certificate (checked by
    /// rustls' own name verification), and a wider request over an existing
    /// pair never mints a new one.
    #[test]
    fn sans_are_fixed_at_generation() {
        let dir = tempfile::tempdir().unwrap();
        let covers = |cert: &Path, name: &str| {
            let certs = load_certs(cert).unwrap();
            let parsed = rustls::server::ParsedCertificate::try_from(&certs[0]).unwrap();
            let server_name = rustls::pki_types::ServerName::try_from(name.to_owned()).unwrap();
            rustls::client::verify_server_name(&parsed, &server_name).is_ok()
        };

        let wide = dir.path().join("wide-cert.pem");
        ensure_self_signed_for(
            &wide,
            &dir.path().join("wide-key.pem"),
            &["100.64.0.2".to_owned()],
        )
        .unwrap();
        for name in ["100.64.0.2", "localhost", "127.0.0.1", "::1"] {
            assert!(covers(&wide, name), "{name} is named");
        }
        assert!(!covers(&wide, "100.64.0.3"));

        let narrow = dir.path().join("narrow-cert.pem");
        let narrow_key = dir.path().join("narrow-key.pem");
        ensure_self_signed(&narrow, &narrow_key).unwrap();
        assert!(!covers(&narrow, "100.64.0.2"), "loopback only");
        let fp = cert_fingerprint(&narrow).unwrap();
        ensure_self_signed_for(&narrow, &narrow_key, &["100.64.0.2".to_owned()]).unwrap();
        assert_eq!(cert_fingerprint(&narrow).unwrap(), fp, "never widened");
    }

    /// A world-readable or group-writable key is refused before its bytes are
    /// read; a group-readable one (an `ssl-cert` group setup) still loads.
    #[test]
    fn loading_a_key_checks_its_mode() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        ensure_self_signed(&cert, &key).unwrap();
        for (mode, loads) in [(0o600, true), (0o640, true), (0o644, false), (0o660, false)] {
            fs::set_permissions(&key, fs::Permissions::from_mode(mode)).unwrap();
            let result = load_key(&key);
            assert_eq!(result.is_ok(), loads, "{mode:04o}: {result:?}");
            if !loads {
                assert!(
                    matches!(&result, Err(CertError::InsecureKey(message)) if message.contains("chmod 600")),
                    "{mode:04o}: {result:?}"
                );
            }
        }
    }

    #[test]
    fn loading_reports_missing_and_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.pem");
        assert!(load_certs(&missing).is_err());
        assert!(cert_fingerprint(&missing).is_err());
        assert!(load_key(&missing).is_err());

        let empty = dir.path().join("empty.pem");
        fs::write(&empty, "").unwrap();
        let err = load_certs(&empty).unwrap_err();
        assert!(
            matches!(&err, CertError::NoCerts(path) if path == &empty.display().to_string()),
            "{err}"
        );
    }
}
