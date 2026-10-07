//! TLS for the remote listeners (ADR-0031).
//!
//! The `wss://` acceptor and the QUIC / WebTransport server configs, all on
//! rustls with the `ring` provider selected explicitly. Certificate generation and PEM loading are
//! [`phux_dial::cert`]'s (shared with `phux-relay`, ADR-0051); what stays here
//! is the server's configs plus the ADR-0091 name-coverage *reporting*
//! surface. An existing certificate is never widened: a new SAN means a new
//! fingerprint, which un-pairs every device.

use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use phux_dial::cert;
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, ServerName};
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
    /// The workload CA could not produce a client-certificate verifier.
    #[error("client certificate verifier: {0}")]
    ClientVerifier(String),
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
    /// A development build aimed at the production certificate or key
    /// ([`refuse_dev_on_production_tls`]).
    #[error("{0}")]
    ProductionState(String),
    /// The private key is owned by another account, or other accounts can
    /// read or replace it.
    #[error("{0}")]
    InsecureKey(String),
}

/// Maps onto the variants [`TlsError`] already had, so operator-facing
/// messages keep this crate's wording rather than the shared crate's.
impl From<cert::CertError> for TlsError {
    fn from(err: cert::CertError) -> Self {
        match err {
            cert::CertError::Io(err) => Self::Io(err),
            cert::CertError::Rcgen(err) => Self::Rcgen(err),
            cert::CertError::Pem(err) => Self::Pem(err),
            cert::CertError::NoCerts(path) => Self::NoCerts(path),
            cert::CertError::InsecureKey(message) => Self::InsecureKey(message),
            cert::CertError::PartialTlsPair { present, missing } => {
                Self::PartialTlsPair { present, missing }
            }
        }
    }
}

/// Default persisted path for the auto-generated remote-consumer certificate.
#[must_use]
pub fn default_cert_path() -> PathBuf {
    crate::telemetry::state_dir().join("remote-cert.pem")
}

/// Default persisted path for the auto-generated remote-consumer private key.
#[must_use]
pub fn default_key_path() -> PathBuf {
    crate::telemetry::state_dir().join("remote-key.pem")
}

/// Provision a self-signed pair naming only the loopback identities, if
/// either file is missing. Prefer [`ensure_self_signed_for`] when the
/// routable address is known.
pub fn ensure_self_signed(cert_path: &Path, key_path: &Path) -> Result<(), TlsError> {
    ensure_self_signed_for(cert_path, key_path, &[])
}

/// Provision a self-signed pair naming `advertised` alongside loopback.
///
/// Entries may be IP literals or DNS names, and nothing happens unless a
/// file is missing. The key is written owner-only. A complete pair is left untouched, so the
/// pinned fingerprint is stable; coverage gaps are reported by
/// [`covers_name`] instead of repaired (ADR-0091).
pub fn ensure_self_signed_for(
    cert_path: &Path,
    key_path: &Path,
    advertised: &[String],
) -> Result<(), TlsError> {
    refuse_dev_on_production_tls(cert_path, key_path)?;
    Ok(cert::ensure_self_signed_for(
        cert_path, key_path, advertised,
    )?)
}

/// Provision the server's own TLS pair when neither file exists.
///
/// The certificate is the one the workload CA at `authority` issues, written as the chain
/// (leaf, then CA) so clients can pin the CA (ADR-0153). The CA is created
/// first when it does not exist (first routable listen, ADR-0116). An existing
/// pair is never touched, so a self-signed leaf a device pins keeps working;
/// a half-present pair is refused as by [`ensure_self_signed_for`].
///
/// When the authority cannot issue (an insecure state directory, a partial
/// CA pair), this logs why and provisions a self-signed pair instead, so a
/// listener is never lost to it; clients then pin that leaf, as before.
///
/// # Errors
///
/// As [`ensure_self_signed_for`].
pub fn ensure_server_identity(
    cert_path: &Path,
    key_path: &Path,
    advertised: &[String],
    authority: &crate::workload::WorkloadPaths,
) -> Result<(), TlsError> {
    refuse_dev_on_production_tls(cert_path, key_path)?;
    match (cert_path.exists(), key_path.exists()) {
        (true, true) => return Ok(()),
        (false, false) => {}
        // The shared provisioner names the survivor.
        _ => return ensure_self_signed_for(cert_path, key_path, advertised),
    }
    match crate::workload::issue_server_identity(authority, &cert::san_list(advertised)) {
        Ok(issued) => Ok(cert::write_pair(
            cert_path,
            key_path,
            issued.chain_pem(),
            issued.key_pem(),
        )?),
        Err(error) => {
            tracing::warn!(
                %error,
                "the workload authority could not issue the server certificate; \
                 provisioning a self-signed one, which clients pin by its leaf"
            );
            ensure_self_signed_for(cert_path, key_path, advertised)
        }
    }
}

/// The `sha256:` fingerprint of the certificate authority the certificate
/// file at `cert_path` presents after its leaf, when the leaf chains to it
/// (ADR-0153); `None` for a self-signed or operator-supplied leaf.
///
/// # Errors
///
/// The certificate file cannot be read or parsed.
pub fn presented_authority(cert_path: &Path) -> Result<Option<String>, TlsError> {
    let certs = cert::load_certs(cert_path)?;
    let Some((leaf, rest)) = certs.split_first() else {
        return Err(TlsError::NoCerts(cert_path.display().to_string()));
    };
    Ok(phux_dial::tls::chain_authority(
        leaf,
        rest,
        rustls::pki_types::UnixTime::now(),
    ))
}

/// Refuse a development build the production certificate or private key.
///
/// Provisioning one would write production state, and presenting one would
/// let a dev server answer as the production identity. The one check lives
/// in [`phux_config::production::refuse_dev_on_production_state`].
///
/// # Errors
///
/// [`TlsError::ProductionState`] with the refusal.
pub fn refuse_dev_on_production_tls(cert_path: &Path, key_path: &Path) -> Result<(), TlsError> {
    for path in [cert_path, key_path] {
        phux_config::production::refuse_dev_on_production_state(path)
            .map_err(TlsError::ProductionState)?;
    }
    Ok(())
}

/// The SANs a listener bound to `addr` advertises.
///
/// Its own address when a remote consumer could dial it. Loopback is always covered, and a wildcard
/// bind names no address (`phux pair` passes the overlay in directly).
#[must_use]
pub fn advertised_for_bind(addr: std::net::SocketAddr) -> Vec<String> {
    let ip = addr.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        Vec::new()
    } else {
        vec![san_name(ip)]
    }
}

/// The SAN form of an address: a bare, unbracketed IP literal.
#[must_use]
pub fn san_name(addr: IpAddr) -> String {
    addr.to_string()
}

/// Whether a name-validating client would accept `name` for `cert_path`.
///
/// Runs rustls' own webpki name check. phux's
/// consumers pin the fingerprint and ignore the name, so this reports the
/// third-party path. `Err` means the certificate could not be read; an
/// uncovered or unparseable name is `Ok(false)`.
pub fn covers_name(cert_path: &Path, name: &str) -> Result<bool, TlsError> {
    let certs = cert::load_certs(cert_path)?;
    let leaf = certs
        .first()
        .ok_or_else(|| TlsError::NoCerts(cert_path.display().to_string()))?;
    let parsed = rustls::server::ParsedCertificate::try_from(leaf)?;
    let Ok(server_name) = ServerName::try_from(name.to_owned()) else {
        return Ok(false);
    };
    Ok(rustls::client::verify_server_name(&parsed, &server_name).is_ok())
}

/// Which of `names` the certificate at `cert_path` does **not** cover, in
/// order. Empty when every name verifies.
pub fn uncovered_names(cert_path: &Path, names: &[String]) -> Result<Vec<String>, TlsError> {
    let mut missing = Vec::new();
    for name in names {
        if !covers_name(cert_path, name)? {
            missing.push(name.clone());
        }
    }
    Ok(missing)
}

/// ALPN protocol id advertised on the QUIC listener, owned by the wire crate
/// so listener and dialer cannot drift.
pub(crate) use phux_protocol::policy::QUIC_ALPN;

/// Build a [`TlsAcceptor`] for the WebSocket listener from a PEM cert + key.
pub fn acceptor_from_pem(cert_path: &Path, key_path: &Path) -> Result<TlsAcceptor, TlsError> {
    acceptor_from_pem_with_client_ca(cert_path, key_path, None)
}

/// A WebSocket TLS acceptor that, with `client_ca`, verifies client
/// certificates against the workload CA.
pub(crate) fn acceptor_from_pem_with_client_ca(
    cert_path: &Path,
    key_path: &Path,
    client_ca: Option<&CertificateDer<'static>>,
) -> Result<TlsAcceptor, TlsError> {
    Ok(TlsAcceptor::from(Arc::new(server_config(
        cert_path, key_path, client_ca, false, None,
    )?)))
}

/// The QUIC server config (TLS 1.3, phux ALPN), with optional workload-CA
/// client verification.
pub(crate) fn quic_server_config_with_client_ca(
    cert_path: &Path,
    key_path: &Path,
    client_ca: Option<&CertificateDer<'static>>,
) -> Result<ServerConfig, TlsError> {
    server_config(cert_path, key_path, client_ca, true, Some(QUIC_ALPN))
}

/// The WebTransport server config: TLS 1.3 with the standard `h3` ALPN,
/// since browsers offer exactly `h3`.
#[cfg(feature = "webtransport")]
pub(crate) fn webtransport_server_config(
    cert_path: &Path,
    key_path: &Path,
) -> Result<ServerConfig, TlsError> {
    server_config(cert_path, key_path, None, true, Some(b"h3"))
}

/// One rustls server config over the shared cert material. QUIC forbids
/// anything below TLS 1.3; the `wss://` acceptor takes rustls' safe defaults.
fn server_config(
    cert_path: &Path,
    key_path: &Path,
    client_ca: Option<&CertificateDer<'static>>,
    tls13_only: bool,
    alpn: Option<&[u8]>,
) -> Result<ServerConfig, TlsError> {
    let certs = cert::load_certs(cert_path)?;
    let key = cert::load_key(key_path)?;
    let builder =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()));
    let builder = if tls13_only {
        builder.with_protocol_versions(&[&rustls::version::TLS13])
    } else {
        builder.with_safe_default_protocol_versions()
    }
    .map_err(TlsError::Rustls)?;
    let mut config = match client_ca {
        Some(ca) => builder
            .with_client_cert_verifier(client_verifier(ca)?)
            .with_single_cert(certs, key)?,
        None => builder.with_no_client_auth().with_single_cert(certs, key)?,
    };
    if let Some(alpn) = alpn {
        config.alpn_protocols = vec![alpn.to_vec()];
    }
    Ok(config)
}

/// The client-certificate verifier for a workload CA, built from the DER the
/// workload store already read. `phux workload add-key` uses the same one,
/// so enrollment accepts exactly what the handshake accepts.
pub(crate) fn client_verifier(
    ca: &CertificateDer<'static>,
) -> Result<Arc<dyn rustls::server::danger::ClientCertVerifier>, TlsError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.clone()).map_err(TlsError::Rustls)?;
    rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|error| TlsError::ClientVerifier(error.to_string()))
}

/// SHA-256 fingerprint of the leaf certificate as uppercase colon-separated
/// hex, the out-of-band pin `phux pair` shows.
pub fn cert_fingerprint(cert_path: &Path) -> Result<String, TlsError> {
    Ok(cert::cert_fingerprint(cert_path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn fresh_pair() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("remote-cert.pem");
        let key = dir.path().join("remote-key.pem");
        ensure_self_signed(&cert, &key).unwrap();
        (dir, cert, key)
    }

    #[test]
    fn ensure_self_signed_refuses_a_partial_pair() {
        let (_dir, cert, key) = fresh_pair();
        let fp = cert_fingerprint(&cert).unwrap();

        // Key lost, cert survives: refuse rather than rotate the pinned
        // fingerprint.
        fs::remove_file(&key).unwrap();
        let err = ensure_self_signed(&cert, &key).unwrap_err();
        assert!(matches!(err, TlsError::PartialTlsPair { .. }), "{err}");
        assert_eq!(cert_fingerprint(&cert).unwrap(), fp);

        fs::remove_file(&cert).unwrap();
        fs::write(&key, "not-a-real-key").unwrap();
        let err = ensure_self_signed(&cert, &key).unwrap_err();
        assert!(matches!(err, TlsError::PartialTlsPair { .. }), "{err}");
    }

    #[test]
    fn ensure_self_signed_provisions_then_is_idempotent_and_builds() {
        let (dir, cert, key) = fresh_pair();
        let key_mode = fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(key_mode, 0o600, "private key must be owner-only");

        let fp1 = cert_fingerprint(&cert).unwrap();
        ensure_self_signed(&cert, &key).unwrap();
        assert_eq!(fp1, cert_fingerprint(&cert).unwrap());
        assert_eq!(fp1.matches(':').count(), 31);
        assert!(fp1.bytes().all(|b| b.is_ascii_hexdigit() || b == b':'));
        acceptor_from_pem(&cert, &key).unwrap();

        let missing = dir.path().join("nope.pem");
        assert!(acceptor_from_pem(&missing, &missing).is_err());
        assert!(cert_fingerprint(&missing).is_err());
    }

    /// An operator key (`PHUX_WS_TLS_KEY`) other accounts can read is refused
    /// before any listener serves with it; a group-readable one (an
    /// `ssl-cert` group setup) is used, with a warning.
    #[test]
    fn a_world_readable_key_is_refused_and_a_group_readable_one_served() {
        let (_dir, cert, key) = fresh_pair();
        fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).unwrap();
        let Err(err) = acceptor_from_pem(&cert, &key) else {
            panic!("a world-readable key must be refused");
        };
        assert!(matches!(err, TlsError::InsecureKey(_)), "{err}");
        assert!(quic_server_config_with_client_ca(&cert, &key, None).is_err());

        fs::set_permissions(&key, fs::Permissions::from_mode(0o640)).unwrap();
        acceptor_from_pem(&cert, &key).unwrap();
        quic_server_config_with_client_ca(&cert, &key, None).unwrap();
    }

    #[test]
    fn advertised_for_bind_names_only_a_dialable_address() {
        let advertised = |s: &str| advertised_for_bind(s.parse().unwrap());
        assert_eq!(advertised("100.64.0.2:8787"), vec!["100.64.0.2"]);
        assert_eq!(
            advertised("[fd7a:115c:a1e0::1]:8787"),
            vec!["fd7a:115c:a1e0::1"]
        );
        for bind in ["127.0.0.1:8787", "[::1]:8787", "0.0.0.0:8787", "[::]:8787"] {
            assert!(advertised(bind).is_empty(), "{bind}");
        }
    }

    /// ADR-0091: a wider request never reissues an existing certificate; the
    /// gap is reported instead.
    #[test]
    fn an_existing_cert_is_never_widened() {
        let (_dir, cert, key) = fresh_pair();
        let fp = cert_fingerprint(&cert).unwrap();

        ensure_self_signed_for(&cert, &key, &["100.64.0.2".to_owned()]).unwrap();
        assert_eq!(cert_fingerprint(&cert).unwrap(), fp);
        assert!(!covers_name(&cert, "100.64.0.2").unwrap());
        assert_eq!(
            uncovered_names(&cert, &["100.64.0.2".to_owned(), "127.0.0.1".to_owned()]).unwrap(),
            vec!["100.64.0.2"],
        );
        // An unparseable name is uncovered, not a certificate error.
        assert!(!covers_name(&cert, "not a valid name").unwrap());
        assert!(!covers_name(&cert, "").unwrap());
    }

    #[test]
    fn m_tls_acceptor_and_quic_config_accept_a_workload_ca() {
        let (dir, cert, key) = fresh_pair();
        let ca = dir.path().join("workload-ca.pem");
        let ca_key = dir.path().join("workload-ca.key");
        crate::workload::ensure_ca(&ca, &ca_key).unwrap();
        let ca = crate::workload::authority_certificate(&ca).unwrap();
        acceptor_from_pem_with_client_ca(&cert, &key, Some(&ca)).unwrap();
        quic_server_config_with_client_ca(&cert, &key, Some(&ca)).unwrap();
    }
}
