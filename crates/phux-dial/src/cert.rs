//! Self-signed certificate provisioning for phux TLS listeners.
//!
//! The counterpart to [`crate::tls`]'s pinning. This crate already owns the
//! *consumer* half of the phux trust story — the fingerprint format, its
//! normalization, and the verifier that compares it — so it owns the
//! *provisioning* half too: the generator that mints the certificate whose
//! fingerprint that verifier checks, and the loader that reads it back.
//!
//! Two listeners provision on exactly these terms and used to do it with
//! near-identical private copies: `phux-server`'s remote-consumer listener
//! (ADR-0031) and `phux-relay`'s single QUIC endpoint (ADR-0051). ADR-0051
//! forbids the relay depending on `phux-server`, and both already depend on
//! this crate, so this is where the one implementation lives.
//!
//! The contract every caller relies on:
//!
//! - **A complete pair is never touched.** The SHA-256 fingerprint is the
//!   trust anchor, pinned out-of-band on devices phux cannot reach;
//!   regenerating would rotate it and silently un-pair every one of them.
//! - **A half-present pair is refused**, not repaired, for the same reason —
//!   the operator deletes the survivor deliberately.
//! - **SANs are chosen once, at generation** (ADR-0091). An existing
//!   certificate is never widened; coverage is reported by the caller.
//! - **The key is owner-only (`0o600`); the certificate is public.**
//!
//! Callers keep their own error vocabulary and map [`CertError`] into it, so
//! the messages an operator sees are unchanged by living here.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};

/// Errors from provisioning or reading persisted TLS material.
///
/// Deliberately narrower than either caller's error enum: this type covers
/// only what generation and loading can fail at. Building a rustls config
/// from the loaded material is the caller's business, and so is
/// `rustls::Error`.
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
    /// Exactly one of the persisted cert/key pair exists. Regenerating would
    /// silently rotate the fingerprint pinned on every paired device, so the
    /// operator must delete the survivor explicitly.
    #[error("partial TLS pair: {present} exists but {missing} is missing")]
    PartialTlsPair {
        /// Path of the file that still exists.
        present: String,
        /// Path of the file that is missing.
        missing: String,
    },
}

/// SANs every generated certificate carries, whatever else it names: the
/// loopback identities the local dev path dials.
pub const LOOPBACK_SANS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

/// Provision a self-signed certificate + key at the given paths if either is
/// missing, naming only the loopback identities.
///
/// The address-agnostic form, for call sites with no advertised address to
/// name (tests, and any provisioning that happens before an address is known).
/// Prefer [`ensure_self_signed_for`] wherever the routable address *is* known:
/// SANs can only be chosen when the certificate is minted.
pub fn ensure_self_signed(cert_path: &Path, key_path: &Path) -> Result<(), CertError> {
    ensure_self_signed_for(cert_path, key_path, &[])
}

/// Provision a self-signed certificate + key at the given paths if either is
/// missing, naming `advertised` in the SANs alongside the loopback identities.
///
/// This is what lets a remote listener need no operator cert setup (ADR-0031
/// "seamless"). A complete pair is left untouched, so the fingerprint stays
/// stable across restarts once pinned on a device.
///
/// The certificate is public (world-readable); the private key is written
/// owner-only (`0o600`). SANs always cover `localhost`/`127.0.0.1`/`::1`;
/// `advertised` adds the addresses phux hands out — the listener's own
/// routable bind and the overlay address `phux pair` embeds in its connect
/// link. Each entry may be an IP literal or a DNS name; rcgen classifies it by
/// whether it parses as an address. Duplicates and empties are dropped, and
/// order is preserved so the SAN list is a stable function of its input.
///
/// **A certificate that already exists is never widened** (ADR-0091). Adding a
/// SAN means minting a new certificate, which means a new fingerprint, which
/// un-pairs every device that pinned the old one — a silent, total trust break
/// traded for a handshake convenience. Coverage is therefore *reported* rather
/// than repaired, by the caller that knows what it advertises.
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
    match (cert_path.exists(), key_path.exists()) {
        (true, true) => return Ok(()),
        (false, false) => {}
        // One survivor: regenerating would silently rotate the pinned
        // fingerprint out from under every paired device. Refuse and make the
        // operator delete the survivor deliberately.
        (true, false) => {
            return Err(CertError::PartialTlsPair {
                present: cert_path.display().to_string(),
                missing: key_path.display().to_string(),
            });
        }
        (false, true) => {
            return Err(CertError::PartialTlsPair {
                present: key_path.display().to_string(),
                missing: cert_path.display().to_string(),
            });
        }
    }
    let certified = rcgen::generate_simple_self_signed(san_list(advertised))?;
    if let Some(parent) = cert_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(cert_path, certified.cert.pem())?;
    let mut key_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(key_path)?;
    io::Write::write_all(&mut key_file, certified.key_pair.serialize_pem().as_bytes())?;
    Ok(())
}

/// The SAN list for a freshly minted certificate: [`LOOPBACK_SANS`] then the
/// advertised names, de-duplicated with order preserved.
///
/// Empty entries are dropped rather than passed through — rcgen would reject
/// `""` as a DNS name and fail provisioning outright, and an empty advertised
/// address is a caller with nothing to say, not an error worth taking the
/// listener down for.
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

/// SHA-256 fingerprint of the leaf certificate, as uppercase
/// colon-separated hex (`AB:CD:…`).
///
/// The conventional shape for an out-of-band pin shown alongside a pairing
/// token, and the shape [`crate::tls::CertTrust::Pinned`] accepts.
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

/// Read the PEM certificate chain: leaf first, then any intermediates.
///
/// # Errors
///
/// [`CertError::Pem`] if the file cannot be read or parsed, and
/// [`CertError::NoCerts`] if it parses but holds nothing — an empty chain is
/// not a usable listener, so it is refused here rather than at handshake time.
pub fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, CertError> {
    let certs = CertificateDer::pem_file_iter(path)?.collect::<Result<Vec<_>, _>>()?;
    if certs.is_empty() {
        return Err(CertError::NoCerts(path.display().to_string()));
    }
    Ok(certs)
}

/// Read the first PEM private key (PKCS#8, SEC1, or PKCS#1).
///
/// # Errors
///
/// [`CertError::Pem`] if the file cannot be read or holds no private key.
pub fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>, CertError> {
    Ok(PrivateKeyDer::from_pem_file(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// The generated pair is usable material, not just files on disk: it loads
    /// back through the same loaders a listener uses, and the leaf is what the
    /// fingerprint is taken over.
    #[test]
    fn a_generated_pair_round_trips_through_the_loaders() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        ensure_self_signed(&cert, &key).unwrap();

        let certs = load_certs(&cert).unwrap();
        assert_eq!(certs.len(), 1, "a self-signed leaf, no intermediates");
        load_key(&key).unwrap();

        // The fingerprint is the SHA-256 of the leaf DER, in the shape
        // `phux pair` prints and `CertTrust::Pinned` accepts.
        let digest = Sha256::digest(certs[0].as_ref());
        let expected: Vec<String> = digest.iter().map(|b| format!("{b:02X}")).collect();
        assert_eq!(cert_fingerprint(&cert).unwrap(), expected.join(":"));
    }

    /// The pin must survive restarts: provisioning twice must not regenerate.
    #[test]
    fn provisioning_is_idempotent_so_the_pin_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");

        ensure_self_signed(&cert, &key).unwrap();
        assert!(cert.exists() && key.exists());
        let first = cert_fingerprint(&cert).unwrap();
        ensure_self_signed(&cert, &key).unwrap();
        assert_eq!(cert_fingerprint(&cert).unwrap(), first);

        // Shape: 32 SHA-256 bytes as uppercase colon-separated hex pairs.
        assert_eq!(first.matches(':').count(), 31);
        assert_eq!(first.len(), 32 * 3 - 1);
        assert!(
            first
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_lowercase() || b == b':')
        );
    }

    /// The private key is secret material and must not be group- or
    /// world-readable, however permissive the ambient umask is.
    #[test]
    fn the_key_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        ensure_self_signed(&cert, &key).unwrap();

        let mode = fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "private key must be owner-only");
    }

    /// Provisioning creates the state directory it was pointed at.
    #[test]
    fn missing_parent_directories_are_created() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        let cert = nested.join("cert.pem");
        let key = nested.join("key.pem");
        ensure_self_signed(&cert, &key).unwrap();
        assert!(cert.exists() && key.exists());
    }

    /// Either survivor of a broken pair is refused, in both directions, and
    /// the surviving file is left exactly as it was — the whole point is that
    /// the pinned fingerprint is not rotated behind the operator's back.
    #[test]
    fn a_partial_pair_is_refused_from_either_side() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        ensure_self_signed(&cert, &key).unwrap();
        let fp = cert_fingerprint(&cert).unwrap();

        // Key lost, cert survives.
        fs::remove_file(&key).unwrap();
        let err = ensure_self_signed(&cert, &key).unwrap_err();
        let CertError::PartialTlsPair { present, missing } = &err else {
            panic!("expected PartialTlsPair, got {err}");
        };
        assert_eq!(present, &cert.display().to_string());
        assert_eq!(missing, &key.display().to_string());
        assert_eq!(cert_fingerprint(&cert).unwrap(), fp, "cert untouched");

        // Cert lost, key survives: the same refusal, with the roles swapped.
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

        // Advertised names follow the loopback set, in the order given.
        assert_eq!(
            san_list(&["100.64.0.2".to_owned(), "mini.tail.ts.net".to_owned()]),
            vec![
                "localhost",
                "127.0.0.1",
                "::1",
                "100.64.0.2",
                "mini.tail.ts.net"
            ]
        );

        // A repeat of a loopback name, a repeat of an advertised name, and an
        // all-whitespace entry all collapse away. rcgen rejects an empty DNS
        // name outright, so letting one through would fail provisioning.
        assert_eq!(
            san_list(&[
                "127.0.0.1".to_owned(),
                "  ".to_owned(),
                "100.64.0.2".to_owned(),
                "100.64.0.2".to_owned(),
            ]),
            vec!["localhost", "127.0.0.1", "::1", "100.64.0.2"]
        );
    }

    /// ADR-0091: SANs are a function of the moment of generation. Asking for a
    /// wider set over an existing pair must not mint a new certificate.
    #[test]
    fn an_existing_cert_is_never_widened() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");

        ensure_self_signed(&cert, &key).unwrap();
        let fp = cert_fingerprint(&cert).unwrap();
        ensure_self_signed_for(&cert, &key, &["100.64.0.2".to_owned()]).unwrap();
        assert_eq!(
            cert_fingerprint(&cert).unwrap(),
            fp,
            "the pinned fingerprint must survive a wider request"
        );
    }

    /// The advertised address reaches the certificate (the f3bb7a86 /
    /// ADR-0091 behaviour), proven the way that commit proved it: by the name
    /// check a rustls client actually runs, not by a SAN-string comparison.
    #[test]
    fn a_generated_cert_covers_the_advertised_address_and_loopback() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        ensure_self_signed_for(&cert, &key, &["100.64.0.2".to_owned()]).unwrap();

        let certs = load_certs(&cert).unwrap();
        let parsed = rustls::server::ParsedCertificate::try_from(&certs[0]).unwrap();
        let covers = |name: &str| {
            let server_name = rustls::pki_types::ServerName::try_from(name.to_owned()).unwrap();
            rustls::client::verify_server_name(&parsed, &server_name).is_ok()
        };
        assert!(covers("100.64.0.2"), "the advertised address is named");
        assert!(covers("localhost"), "loopback names are unconditional");
        assert!(covers("127.0.0.1"));
        assert!(covers("::1"));
        assert!(!covers("100.64.0.3"), "an unrelated address is not");
    }

    /// The address-agnostic form names loopback and nothing else — this is the
    /// narrow certificate ADR-0091 exists to explain.
    #[test]
    fn the_address_agnostic_form_names_only_loopback() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        ensure_self_signed(&cert, &key).unwrap();

        let certs = load_certs(&cert).unwrap();
        let parsed = rustls::server::ParsedCertificate::try_from(&certs[0]).unwrap();
        let name = rustls::pki_types::ServerName::try_from("100.64.0.2".to_owned()).unwrap();
        assert!(rustls::client::verify_server_name(&parsed, &name).is_err());
    }

    /// Two independently generated certificates differ, so a fingerprint
    /// actually identifies one server rather than the phux generator.
    #[test]
    fn separate_provisionings_produce_distinct_fingerprints() {
        let dir = tempfile::tempdir().unwrap();
        let mint = |name: &str| {
            let cert = dir.path().join(format!("{name}-cert.pem"));
            let key = dir.path().join(format!("{name}-key.pem"));
            ensure_self_signed(&cert, &key).unwrap();
            cert_fingerprint(&cert).unwrap()
        };
        assert_ne!(mint("a"), mint("b"));
    }

    #[test]
    fn loading_reports_missing_and_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.pem");
        assert!(load_certs(&missing).is_err());
        assert!(cert_fingerprint(&missing).is_err());
        assert!(load_key(&missing).is_err());

        // A readable file with no PEM certificate in it is `NoCerts`, naming
        // the path, rather than a silently empty chain.
        let empty = dir.path().join("empty.pem");
        fs::write(&empty, "").unwrap();
        let err = load_certs(&empty).unwrap_err();
        assert!(
            matches!(&err, CertError::NoCerts(path) if path == &empty.display().to_string()),
            "{err}"
        );
    }
}
