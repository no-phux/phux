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
use std::sync::{Arc, Mutex, PoisonError};

use phux_dial::cert;
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::server::danger::ClientCertVerifier;
use tokio_rustls::TlsAcceptor;

use crate::workload::ClientRoots;

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
    /// The workload CA could not issue the server certificate; there is no
    /// self-signed fallback (ADR-0179).
    #[error("the workload CA could not issue the server certificate: {0}")]
    Authority(String),
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

/// The operator's TLS pair from `PHUX_WS_TLS_CERT` / `PHUX_WS_TLS_KEY`, or
/// `(None, None)` when neither is set or both name the default paths.
///
/// A default path is not an operator choice: `phux service install` once baked
/// the defaults into the unit, and treating them as operator-supplied meant
/// the server never provisioned a missing pair and `phux pair` could not
/// recover without a listener (#1766).
#[must_use]
pub fn operator_tls_paths(
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> (Option<PathBuf>, Option<PathBuf>) {
    let cert = lookup("PHUX_WS_TLS_CERT").map(PathBuf::from);
    let key = lookup("PHUX_WS_TLS_KEY").map(PathBuf::from);
    let is_default = cert.as_deref().is_none_or(|c| c == default_cert_path())
        && key.as_deref().is_none_or(|k| k == default_key_path());
    if is_default {
        (None, None)
    } else {
        (cert, key)
    }
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
/// CA pair), this fails: a new pair no CA issued is a server no client can
/// pin by its CA, so nothing falls back to a self-signed one (ADR-0179).
///
/// # Errors
///
/// As [`ensure_self_signed_for`], or [`TlsError::Authority`] when the
/// workload CA cannot issue.
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
        Err(error) => Err(TlsError::Authority(error.to_string())),
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
/// certificates against those roots and requires one in the handshake.
pub(crate) fn acceptor_from_pem_with_client_ca(
    cert_path: &Path,
    key_path: &Path,
    client_ca: Option<&ClientRoots>,
) -> Result<TlsAcceptor, TlsError> {
    let auth = client_ca.map_or(ClientAuth::None, ClientAuth::Required);
    Ok(TlsAcceptor::from(Arc::new(server_config(
        cert_path,
        key_path,
        auth,
        false,
        &[],
    )?)))
}

/// The QUIC server config (TLS 1.3, phux ALPN), with optional workload-CA
/// client verification and, with `enrollment`, the enrollment ALPN beside it.
///
/// With a workload CA the handshake asks for a client certificate and
/// verifies any presented, but completes without one: the enrollment ALPN
/// answers devices that hold none yet, and the listener refuses a terminal
/// connection with no enrolled certificate before reading any stream
/// (`docs/remote.md#who-can-connect`, ADR-0154).
pub(crate) fn quic_server_config_with_client_ca(
    cert_path: &Path,
    key_path: &Path,
    client_ca: Option<&ClientRoots>,
    enrollment: bool,
) -> Result<ServerConfig, TlsError> {
    use phux_protocol::policy::{COORD_ALPN, ENROLL_ALPN, SATELLITE_ALPN};
    let auth = client_ca.map_or(ClientAuth::None, ClientAuth::Requested);
    // The satellite and coordination ALPNs admit by workload certificate
    // alone (ADR-0182), so only a listener that verifies them offers them.
    let alpns: &[&[u8]] = match (enrollment, client_ca.is_some()) {
        (true, true) => &[QUIC_ALPN, ENROLL_ALPN, SATELLITE_ALPN, COORD_ALPN],
        (true, false) => &[QUIC_ALPN, ENROLL_ALPN],
        (false, _) => &[QUIC_ALPN],
    };
    server_config(cert_path, key_path, auth, true, alpns)
}

/// The config of the TLS session a consumer runs inside a relayed stream
/// (ADR-0154 item 5): TLS 1.3, the terminal and enrollment ALPNs, and with a
/// workload CA a client certificate asked for and verified but decided after
/// the handshake, as on the QUIC listener.
pub(crate) fn inner_server_config(
    cert_path: &Path,
    key_path: &Path,
    client_ca: Option<&ClientRoots>,
) -> Result<ServerConfig, TlsError> {
    let auth = client_ca.map_or(ClientAuth::None, ClientAuth::Requested);
    server_config(
        cert_path,
        key_path,
        auth,
        true,
        &[QUIC_ALPN, phux_protocol::policy::ENROLL_ALPN],
    )
}

/// The WebTransport server config: TLS 1.3 with the standard `h3` ALPN,
/// since browsers offer exactly `h3`.
#[cfg(feature = "webtransport")]
pub(crate) fn webtransport_server_config(
    cert_path: &Path,
    key_path: &Path,
) -> Result<ServerConfig, TlsError> {
    server_config(cert_path, key_path, ClientAuth::None, true, &[b"h3"])
}

/// Whether a handshake asks for a workload client certificate.
#[derive(Clone, Copy)]
enum ClientAuth<'a> {
    /// No `CertificateRequest`.
    None,
    /// Asked for, verified when presented, and required by the handshake.
    Required(&'a ClientRoots),
    /// Asked for and verified when presented; the listener decides after the
    /// handshake what a certificate-less connection may do.
    Requested(&'a ClientRoots),
}

/// One rustls server config over the shared cert material. QUIC forbids
/// anything below TLS 1.3; the `wss://` acceptor takes rustls' safe defaults.
fn server_config(
    cert_path: &Path,
    key_path: &Path,
    client_auth: ClientAuth<'_>,
    tls13_only: bool,
    alpns: &[&[u8]],
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
    let mut config = match client_auth {
        ClientAuth::Required(roots) => builder
            .with_client_cert_verifier(WorkloadVerifier::new(roots, false)?)
            .with_single_cert(certs, key)?,
        ClientAuth::Requested(roots) => builder
            .with_client_cert_verifier(WorkloadVerifier::new(roots, true)?)
            .with_single_cert(certs, key)?,
        ClientAuth::None => builder.with_no_client_auth().with_single_cert(certs, key)?,
    };
    config.alpn_protocols = alpns.iter().map(|alpn| alpn.to_vec()).collect();
    Ok(config)
}

/// The client-certificate verifier for a workload CA, built from the DER the
/// workload store already read. `phux workload add-key` uses the same one,
/// so enrollment accepts exactly what the handshake accepts.
pub(crate) fn client_verifier(
    ca: &CertificateDer<'static>,
) -> Result<Arc<dyn ClientCertVerifier>, TlsError> {
    roots_verifier(std::slice::from_ref(ca), false)
}

/// A webpki client-certificate verifier over `roots`; `optional` lets the
/// handshake finish without a certificate, though a presented one must
/// still chain to a root.
pub(crate) fn roots_verifier(
    roots: &[CertificateDer<'static>],
    optional: bool,
) -> Result<Arc<dyn ClientCertVerifier>, TlsError> {
    let mut store = rustls::RootCertStore::empty();
    for root in roots {
        store.add(root.clone()).map_err(TlsError::Rustls)?;
    }
    let builder = rustls::server::WebPkiClientVerifier::builder(Arc::new(store));
    if optional {
        builder.allow_unauthenticated()
    } else {
        builder
    }
    .build()
    .map_err(|error| TlsError::ClientVerifier(error.to_string()))
}

/// A listener's client-certificate verifier: [`roots_verifier`] over the
/// host CA and the organization roots as they are at each handshake, rebuilt
/// when they change (`docs/remote.md#managed-by-an-organization`). It names
/// no root in its `CertificateRequest`: phux clients present their one
/// identity regardless, and the set changes under a running listener.
#[derive(Debug)]
struct WorkloadVerifier {
    roots: ClientRoots,
    optional: bool,
    built: Mutex<Built>,
}

/// The roots a [`WorkloadVerifier`] last built from, and what it built.
type Built = (Arc<[CertificateDer<'static>]>, Arc<dyn ClientCertVerifier>);

impl WorkloadVerifier {
    fn new(roots: &ClientRoots, optional: bool) -> Result<Arc<Self>, TlsError> {
        let current = roots.current();
        let verifier = roots_verifier(&current, optional)?;
        Ok(Arc::new(Self {
            roots: roots.clone(),
            optional,
            built: Mutex::new((current, verifier)),
        }))
    }

    /// The verifier for the roots as they are now. A set that fails to
    /// build keeps the last one, with a warning.
    fn current(&self) -> Arc<dyn ClientCertVerifier> {
        let roots = self.roots.current();
        let mut built = self.built.lock().unwrap_or_else(PoisonError::into_inner);
        if !Arc::ptr_eq(&built.0, &roots) {
            match roots_verifier(&roots, self.optional) {
                Ok(verifier) => *built = (roots, verifier),
                Err(error) => {
                    tracing::warn!(%error, "workload client roots changed but do not verify; keeping the previous set");
                }
            }
        }
        Arc::clone(&built.1)
    }

    fn built(&self) -> Arc<dyn ClientCertVerifier> {
        Arc::clone(&self.built.lock().unwrap_or_else(PoisonError::into_inner).1)
    }
}

impl ClientCertVerifier for WorkloadVerifier {
    fn client_auth_mandatory(&self) -> bool {
        !self.optional
    }

    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        self.current()
            .verify_client_cert(end_entity, intermediates, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.built().verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.built().verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.built().supported_verify_schemes()
    }
}

/// SHA-256 fingerprint of the leaf certificate as uppercase colon-separated
/// hex, the out-of-band pin `phux pair` shows.
pub fn cert_fingerprint(cert_path: &Path) -> Result<String, TlsError> {
    Ok(cert::cert_fingerprint(cert_path)?)
}

#[cfg(test)]
mod tests;
