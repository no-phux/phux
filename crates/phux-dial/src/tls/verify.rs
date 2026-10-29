//! Server-certificate pin policy: fingerprint spellings, chain authority,
//! and the rustls verifier that applies a [`CertTrust`].

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};

use super::{AuthorityChange, AuthorityRefusal, CertTrust, DOWNGRADE_REFUSED};

/// Uppercase hex digits only, so `AB:CD:...`, `abcd...`, and spaced pins
/// compare equal.
pub(super) fn normalize_fingerprint(fingerprint: &str) -> String {
    fingerprint
        .chars()
        .filter(char::is_ascii_hexdigit)
        .flat_map(char::to_uppercase)
        .collect()
}

/// SHA-256 of a leaf certificate as bare uppercase hex.
pub(super) fn leaf_fingerprint(cert: &CertificateDer<'_>) -> String {
    let digest = Sha256::digest(cert.as_ref());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02X}");
    }
    hex
}

/// Prefix of the canonical authority fingerprint (`docs/remote.md#who-can-connect`).
const AUTHORITY_PREFIX: &str = "sha256:";

/// The canonical `sha256:` + lowercase-hex fingerprint of a DER certificate:
/// the spelling `phux workload authority` prints and a client stores.
#[must_use]
pub fn authority_fingerprint(cert: &CertificateDer<'_>) -> String {
    format!(
        "{AUTHORITY_PREFIX}{}",
        leaf_fingerprint(cert).to_ascii_lowercase()
    )
}

/// An authority pin in any accepted spelling, as bare uppercase hex.
fn normalize_authority(pin: &str) -> String {
    let trimmed = pin.trim();
    normalize_fingerprint(trimmed.strip_prefix(AUTHORITY_PREFIX).unwrap_or(trimmed))
}

/// Canonical spelling of an authority pin, for diagnostics.
fn canonical_authority(pin: &str) -> String {
    format!(
        "{AUTHORITY_PREFIX}{}",
        normalize_authority(pin).to_ascii_lowercase()
    )
}

/// The certificate authority a presented chain names (ADR-0153).
///
/// That is its last certificate, when the leaf verifies under it as a trust
/// anchor for server authentication now (signature, validity, key usage; the
/// name is not checked, since phux pins rather than names). `None` for a lone
/// leaf or a chain the leaf does not verify under. The canonical `sha256:`
/// form.
#[must_use]
pub fn chain_authority(
    end_entity: &CertificateDer<'_>,
    intermediates: &[CertificateDer<'_>],
    now: UnixTime,
) -> Option<String> {
    let (candidate, between) = intermediates.split_last()?;
    let provider = rustls::crypto::ring::default_provider();
    let parsed = rustls::server::ParsedCertificate::try_from(end_entity).ok()?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(candidate.clone().into_owned()).ok()?;
    rustls::client::verify_server_cert_signed_by_trust_anchor(
        &parsed,
        &roots,
        between,
        now,
        provider.signature_verification_algorithms.all,
    )
    .ok()?;
    Some(authority_fingerprint(candidate))
}

/// [`CertTrust`] with its pins normalized for comparison.
#[derive(Debug)]
pub(super) enum Policy {
    Skip,
    Leaf { pin: String },
    Authority { ca: String },
}

impl From<&CertTrust> for Policy {
    fn from(trust: &CertTrust) -> Self {
        match trust {
            CertTrust::SkipVerify => Self::Skip,
            CertTrust::Pinned(pin) => Self::Leaf {
                pin: normalize_fingerprint(pin),
            },
            CertTrust::Authority { ca } => Self::Authority {
                ca: normalize_authority(ca),
            },
        }
    }
}

/// The phux server-certificate verifier: applies the [`Policy`] to the
/// presented chain and always verifies handshake signatures. With
/// `cert_requested` it first refuses a server that did not ask for the client
/// certificate.
#[derive(Debug)]
pub(super) struct Verifier {
    pub(super) provider: Arc<rustls::crypto::CryptoProvider>,
    pub(super) policy: Policy,
    /// `Some` for [`TlsClientIdentity::RequirePaired`]: whether this
    /// handshake's `CertificateRequest` has arrived. TLS 1.3 sends it before
    /// the server's Certificate, so it is settled when this verifier runs.
    pub(super) cert_requested: Option<Arc<AtomicBool>>,
    /// Filled when [`Policy::Authority`] refuses the presented chain.
    pub(super) refusal: AuthorityRefusal,
}

impl Verifier {
    /// The leaf pin.
    fn verify_leaf(
        pin: &str,
        end_entity: &CertificateDer<'_>,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual = leaf_fingerprint(end_entity);
        if actual != pin {
            return Err(rustls::Error::General(format!(
                "server certificate fingerprint mismatch (pinned {pin}, got {actual})"
            )));
        }
        Ok(ServerCertVerified::assertion())
    }

    /// The authority pin: the leaf must chain to the pinned authority. A
    /// leaf that chains to none is refused like a changed authority; there is
    /// no leaf beside the pin to fall back on (ADR-0179).
    fn verify_authority(
        &self,
        ca: &str,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let presented = chain_authority(end_entity, intermediates, now);
        let accepted = presented
            .as_deref()
            .is_some_and(|presented| normalize_authority(presented) == ca);
        if accepted {
            return Ok(ServerCertVerified::assertion());
        }
        let change = AuthorityChange {
            pinned: canonical_authority(ca),
            presented,
        };
        let message = change.to_string();
        if let Ok(mut slot) = self.refusal.lock() {
            *slot = Some(change);
        }
        Err(rustls::Error::General(message))
    }
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let Some(requested) = &self.cert_requested
            && !requested.swap(false, Ordering::SeqCst)
        {
            return Err(rustls::Error::General(DOWNGRADE_REFUSED.to_owned()));
        }
        match &self.policy {
            Policy::Skip => Ok(ServerCertVerified::assertion()),
            Policy::Leaf { pin } => Self::verify_leaf(pin, end_entity),
            Policy::Authority { ca } => self.verify_authority(ca, end_entity, intermediates, now),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
