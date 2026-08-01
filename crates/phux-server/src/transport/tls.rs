//! TLS termination for the remote-consumer WebSocket listener (ADR-0031).
//!
//! Encryption for `wss://` remote consumers is TLS 1.3 (with 1.2 as a floor)
//! via rustls, terminated here before the RFC 6455 upgrade. The operator
//! supplies a PEM certificate chain and private key; this module turns them
//! into a [`TlsAcceptor`] the listener wraps each accepted TCP stream in.
//!
//! The `ring` crypto provider is selected explicitly (`builder_with_provider`)
//! rather than relying on a process-default install, both because only `ring`
//! is compiled in and so the choice is visible at the call site.
//!
//! [`cert_fingerprint`] computes the SHA-256 of the leaf certificate for the
//! out-of-band pin shown at pairing time (`phux pair`), closing the
//! trust-on-first-use gap ADR-0031 names.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};
use tokio_rustls::TlsAcceptor;

/// Errors from loading TLS material or building the acceptor.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// A certificate or key file could not be read.
    #[error("tls io: {0}")]
    Io(#[from] io::Error),
    /// The certificate file held no certificates.
    #[error("no certificates in {0}")]
    NoCerts(String),
    /// A PEM certificate or key file could not be parsed.
    #[error("pem: {0}")]
    Pem(#[from] rustls::pki_types::pem::Error),
    /// rustls rejected the certificate/key pair.
    #[error("rustls: {0}")]
    Rustls(#[from] rustls::Error),
    /// Generating the self-signed certificate failed.
    #[error("certificate generation: {0}")]
    Rcgen(#[from] rcgen::Error),
    /// Exactly one of the persisted cert/key pair exists. Regenerating
    /// would silently rotate the fingerprint pinned on every paired
    /// device, so the operator must delete the survivor explicitly.
    #[error(
        "partial TLS pair: {present} exists but {missing} is missing — delete {present} to regenerate (this rotates the pinned fingerprint and breaks existing pins)"
    )]
    PartialTlsPair {
        /// Path of the file that still exists.
        present: String,
        /// Path of the file that is missing.
        missing: String,
    },
}

/// Default persisted path for the auto-generated remote-consumer certificate:
/// `<state-dir>/remote-cert.pem`.
#[must_use]
pub fn default_cert_path() -> PathBuf {
    crate::telemetry::state_dir().join("remote-cert.pem")
}

/// Default persisted path for the auto-generated remote-consumer private key:
/// `<state-dir>/remote-key.pem`.
#[must_use]
pub fn default_key_path() -> PathBuf {
    crate::telemetry::state_dir().join("remote-key.pem")
}

/// Provision a self-signed certificate + key at the given paths if either is
/// missing.
///
/// This is what lets a remote listener need no operator cert setup (ADR-0031
/// "seamless"). A complete pair is left untouched, so the fingerprint stays
/// stable across restarts once pinned on a device.
///
/// The certificate is public (world-readable); the private key is written
/// owner-only (`0o600`). SANs cover `localhost`, `127.0.0.1`, and `::1` — a
/// fingerprint-pinning consumer does not rely on hostname matching, but valid
/// SANs keep a conventionally-validating client working on loopback.
pub fn ensure_self_signed(cert_path: &Path, key_path: &Path) -> Result<(), TlsError> {
    match (cert_path.exists(), key_path.exists()) {
        (true, true) => return Ok(()),
        (false, false) => {}
        // One survivor: regenerating would silently rotate the fingerprint
        // pinned on every paired device. Refuse and make the operator
        // delete the survivor deliberately.
        (true, false) => {
            return Err(TlsError::PartialTlsPair {
                present: cert_path.display().to_string(),
                missing: key_path.display().to_string(),
            });
        }
        (false, true) => {
            return Err(TlsError::PartialTlsPair {
                present: key_path.display().to_string(),
                missing: cert_path.display().to_string(),
            });
        }
    }
    let sans = vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
    ];
    let certified = rcgen::generate_simple_self_signed(sans)?;
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

/// ALPN protocol id advertised on the QUIC listener. Owned by `phux-protocol`
/// (the wire crate) so the server listener and the client dialer cannot drift;
/// re-exported here for the QUIC transport's call sites and tests.
pub(crate) use phux_protocol::policy::QUIC_ALPN;

/// Build a rustls [`ServerConfig`] from a PEM certificate chain and private
/// key, using the `ring` provider. Shared by the WebSocket [`TlsAcceptor`] and
/// the QUIC listener so both terminate TLS with the identical cert material.
///
/// `cert_path` is a PEM file with the leaf certificate first, followed by any
/// intermediates; `key_path` is a PEM file with one PKCS#8 / SEC1 / PKCS#1
/// private key. No client authentication is required — the bearer token
/// (see [`crate::auth`]) is the authentication layer; TLS provides encryption
/// and server identity only. Mutual TLS is the ADR-0031 v0.2 hardening.
fn server_config_from_pem(cert_path: &Path, key_path: &Path) -> Result<ServerConfig, TlsError> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;

    Ok(
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(TlsError::Rustls)?
            .with_no_client_auth()
            .with_single_cert(certs, key)?,
    )
}

/// Build a [`TlsAcceptor`] for the WebSocket listener from a PEM cert + key.
pub fn acceptor_from_pem(cert_path: &Path, key_path: &Path) -> Result<TlsAcceptor, TlsError> {
    Ok(TlsAcceptor::from(Arc::new(server_config_from_pem(
        cert_path, key_path,
    )?)))
}

/// Build the rustls [`ServerConfig`] for the QUIC listener from a PEM cert +
/// key: TLS 1.3 only (QUIC forbids earlier versions) with the phux ALPN set.
///
/// Returned as a bare rustls config; the QUIC transport wraps it in quinn's
/// `QuicServerConfig`. Reuses the same cert material as the WebSocket path.
pub(crate) fn quic_server_config(
    cert_path: &Path,
    key_path: &Path,
) -> Result<ServerConfig, TlsError> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;

    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(TlsError::Rustls)?
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![QUIC_ALPN.to_vec()];
    Ok(config)
}

/// Build the rustls [`ServerConfig`] for the WebTransport listener from a PEM
/// cert + key: TLS 1.3 only (QUIC forbids earlier versions) with the standard
/// HTTP/3 ALPN.
///
/// Unlike the raw-QUIC listener (which advertises the phux-private
/// [`QUIC_ALPN`]), WebTransport rides HTTP/3, and browsers offer exactly
/// `h3` — a private ALPN would fail every browser handshake. Reuses the same
/// cert material as the `wss://` and QUIC paths so one pinned fingerprint
/// covers all three remote transports.
#[cfg(feature = "webtransport")]
pub(crate) fn webtransport_server_config(
    cert_path: &Path,
    key_path: &Path,
) -> Result<ServerConfig, TlsError> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;

    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(TlsError::Rustls)?
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
    // The IANA-registered HTTP/3 ALPN id. wtransport exports the same bytes
    // (`wtransport::tls::WEBTRANSPORT_ALPN`); spelled literally here so this
    // module stays wtransport-free apart from the cfg gate.
    config.alpn_protocols = vec![b"h3".to_vec()];
    Ok(config)
}

/// SHA-256 fingerprint of the leaf certificate, formatted as uppercase
/// colon-separated hex (`AB:CD:…`) — the conventional shape for an
/// out-of-band pin shown alongside a pairing token.
pub fn cert_fingerprint(cert_path: &Path) -> Result<String, TlsError> {
    let certs = load_certs(cert_path)?;
    let leaf = certs
        .first()
        .ok_or_else(|| TlsError::NoCerts(cert_path.display().to_string()))?;
    let digest = Sha256::digest(leaf.as_ref());
    let hex: Vec<String> = digest.iter().map(|b| format!("{b:02X}")).collect();
    Ok(hex.join(":"))
}

/// Read the PEM certificate chain.
fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let certs = CertificateDer::pem_file_iter(path)?.collect::<Result<Vec<_>, _>>()?;
    if certs.is_empty() {
        return Err(TlsError::NoCerts(path.display().to_string()));
    }
    Ok(certs)
}

/// Read the first PEM private key (PKCS#8, SEC1, or PKCS#1).
fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>, TlsError> {
    Ok(PrivateKeyDer::from_pem_file(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn ensure_self_signed_refuses_a_partial_pair() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("remote-cert.pem");
        let key = dir.path().join("remote-key.pem");
        ensure_self_signed(&cert, &key).unwrap();
        let fp = cert_fingerprint(&cert).unwrap();

        // Key lost, cert survives: must refuse, not silently rotate the
        // fingerprint every paired device pins.
        fs::remove_file(&key).unwrap();
        let err = ensure_self_signed(&cert, &key).unwrap_err();
        assert!(matches!(err, TlsError::PartialTlsPair { .. }), "{err}");
        assert_eq!(
            cert_fingerprint(&cert).unwrap(),
            fp,
            "cert must be untouched"
        );

        // Cert lost, key survives: same refusal.
        fs::remove_file(&cert).unwrap();
        fs::write(&key, "not-a-real-key").unwrap();
        let err = ensure_self_signed(&cert, &key).unwrap_err();
        assert!(matches!(err, TlsError::PartialTlsPair { .. }), "{err}");
    }

    #[test]
    fn ensure_self_signed_provisions_then_is_idempotent_and_builds() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("remote-cert.pem");
        let key = dir.path().join("remote-key.pem");

        ensure_self_signed(&cert, &key).unwrap();
        assert!(cert.exists() && key.exists());

        // Private key is owner-only; the certificate is public.
        let key_mode = fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(key_mode, 0o600, "private key must be owner-only");

        // Idempotent: a second call does not regenerate, so the pinned
        // fingerprint stays stable across restarts.
        let fp1 = cert_fingerprint(&cert).unwrap();
        ensure_self_signed(&cert, &key).unwrap();
        let fp2 = cert_fingerprint(&cert).unwrap();
        assert_eq!(fp1, fp2);

        // Fingerprint shape: 32 SHA-256 bytes as colon-separated hex pairs.
        assert_eq!(fp1.matches(':').count(), 31);
        assert!(fp1.bytes().all(|b| b.is_ascii_hexdigit() || b == b':'));

        // The generated material builds a working acceptor.
        acceptor_from_pem(&cert, &key).unwrap();
    }

    #[test]
    fn acceptor_and_fingerprint_error_on_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing_cert = dir.path().join("nope.pem");
        let missing_key = dir.path().join("nope.key");
        assert!(acceptor_from_pem(&missing_cert, &missing_key).is_err());
        assert!(cert_fingerprint(&missing_cert).is_err());
    }
}
