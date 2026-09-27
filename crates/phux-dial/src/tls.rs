//! Shared TLS trust policy for remote dial transports.
//!
//! `phux pair` prints a self-signed certificate fingerprint and the dialer
//! pins it for routable hosts; loopback dev may skip verification while still
//! encrypting.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};

use crate::DialError;

/// How a remote dialer decides to trust the server's TLS certificate.
///
/// Both modes encrypt and verify handshake signatures; they differ only in
/// whether the leaf certificate is checked against a pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertTrust {
    /// Accept the server's certificate without verification. **Loopback dev
    /// only**.
    SkipVerify,
    /// Pin the server's leaf-certificate SHA-256 fingerprint, in the
    /// colon-or-bare hex shape `phux pair` prints.
    Pinned(String),
}

/// Explicit TLS client identity for embedders that must not read process
/// environment configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsClientIdentity {
    /// Send no client certificate; pairing-token auth still applies.
    None,
    /// Load one PEM certificate chain and private key from explicit paths.
    PemFiles {
        /// PEM certificate chain, leaf first.
        certificate: PathBuf,
        /// PEM private key corresponding to the leaf certificate.
        private_key: PathBuf,
    },
}

/// Build a rustls client config with the phux remote trust policy, taking the
/// optional workload identity from `PHUX_WORKLOAD_CERT` / `PHUX_WORKLOAD_KEY`.
///
/// `alpn` is set for QUIC and left unset for WebSocket. Public so every dialer
/// (including server-side listener tests) uses this verifier, not a copy.
pub fn client_config(
    trust: &CertTrust,
    alpn: Option<&[u8]>,
) -> Result<rustls::ClientConfig, DialError> {
    let identity = client_identity_from_env()?;
    client_config_with_identity(trust, &identity, alpn)
}

/// Build a rustls client config from explicit trust and identity inputs;
/// never reads the environment.
///
/// # Errors
///
/// Returns [`DialError::Connect`] when TLS setup fails or an explicit PEM
/// identity cannot be read or parsed.
pub fn client_config_with_identity(
    trust: &CertTrust,
    identity: &TlsClientIdentity,
    alpn: Option<&[u8]>,
) -> Result<rustls::ClientConfig, DialError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(Verifier {
        provider: provider.clone(),
        pin: match trust {
            CertTrust::SkipVerify => None,
            CertTrust::Pinned(fingerprint) => Some(normalize_fingerprint(fingerprint)),
        },
    });

    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|err| DialError::Connect(format!("build TLS client config: {err}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    let mut crypto = if let TlsClientIdentity::PemFiles {
        certificate,
        private_key,
    } = identity
    {
        let certs = CertificateDer::pem_file_iter(certificate)
            .map_err(|err| DialError::Connect(format!("read workload certificate: {err}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| DialError::Connect(format!("read workload certificate: {err}")))?;
        let key = rustls::pki_types::PrivateKeyDer::from_pem_file(private_key)
            .map_err(|err| DialError::Connect(format!("read workload key: {err}")))?;
        builder
            .with_client_auth_cert(certs, key)
            .map_err(|err| DialError::Connect(format!("build workload identity: {err}")))?
    } else {
        builder.with_no_client_auth()
    };
    if let Some(alpn) = alpn {
        crypto.alpn_protocols = vec![alpn.to_vec()];
    }
    Ok(crypto)
}

/// Optional client identity for paired mTLS endpoints. Both variables are
/// required; a half-configured identity fails rather than silently dialing
/// unauthenticated.
fn client_identity_from_env() -> Result<TlsClientIdentity, DialError> {
    let cert = std::env::var_os("PHUX_WORKLOAD_CERT").map(PathBuf::from);
    let key = std::env::var_os("PHUX_WORKLOAD_KEY").map(PathBuf::from);
    match (cert, key) {
        (None, None) => Ok(TlsClientIdentity::None),
        (Some(certificate), Some(private_key)) => Ok(TlsClientIdentity::PemFiles {
            certificate,
            private_key,
        }),
        _ => Err(DialError::Connect(
            "PHUX_WORKLOAD_CERT and PHUX_WORKLOAD_KEY must be set together".to_owned(),
        )),
    }
}

/// Uppercase hex digits only, so `AB:CD:...`, `abcd...`, and spaced pins
/// compare equal.
fn normalize_fingerprint(fingerprint: &str) -> String {
    fingerprint
        .chars()
        .filter(char::is_ascii_hexdigit)
        .flat_map(char::to_uppercase)
        .collect()
}

/// SHA-256 of a leaf certificate as bare uppercase hex.
fn leaf_fingerprint(cert: &CertificateDer<'_>) -> String {
    let digest = Sha256::digest(cert.as_ref());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02X}");
    }
    hex
}

/// The phux server-certificate verifier: checks the leaf against `pin` when
/// set, accepts any leaf otherwise, and always verifies handshake signatures.
#[derive(Debug)]
struct Verifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
    /// Normalized expected fingerprint; `None` is [`CertTrust::SkipVerify`].
    pin: Option<String>,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let Some(expected) = &self.pin else {
            return Ok(ServerCertVerified::assertion());
        };
        let actual = leaf_fingerprint(end_entity);
        if &actual == expected {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "server certificate fingerprint mismatch (pinned {expected}, got {actual})"
            )))
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn normalize_fingerprint_is_separator_and_case_insensitive() {
        for pin in ["ab:CD:12:Ef", "ABCD12EF", "  ab cd 12 ef  "] {
            assert_eq!(normalize_fingerprint(pin), "ABCD12EF", "{pin:?}");
        }
    }

    /// The pin is enforced against a real leaf: its own fingerprint (in any
    /// accepted shape) verifies, any other is refused.
    #[test]
    fn pinned_verifier_accepts_only_the_pinned_leaf() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cert = dir.path().join("cert.pem");
        crate::cert::ensure_self_signed(&cert, &dir.path().join("key.pem")).expect("pair");
        let leaf = crate::cert::load_certs(&cert).expect("certs").remove(0);
        let pinned = crate::cert::cert_fingerprint(&cert).expect("fingerprint");
        let verify = |pin: Option<String>| {
            Verifier {
                provider: Arc::new(rustls::crypto::ring::default_provider()),
                pin: pin.map(|p| normalize_fingerprint(&p)),
            }
            .verify_server_cert(
                &leaf,
                &[],
                &ServerName::try_from("localhost").expect("name"),
                &[],
                UnixTime::now(),
            )
        };
        assert!(verify(Some(pinned.clone())).is_ok());
        assert!(verify(Some(pinned.to_lowercase().replace(':', ""))).is_ok());
        assert!(verify(Some("00".repeat(32))).is_err());
        assert!(verify(None).is_ok(), "skip-verify accepts any leaf");
    }

    #[test]
    fn explicit_identity_paths_fail_as_explicit_inputs() {
        let error = client_config_with_identity(
            &CertTrust::SkipVerify,
            &TlsClientIdentity::PemFiles {
                certificate: PathBuf::from("/definitely/missing/phux-cert.pem"),
                private_key: PathBuf::from("/definitely/missing/phux-key.pem"),
            },
            None,
        )
        .expect_err("missing explicit identity");
        assert!(error.to_string().contains("read workload certificate"));
    }

    #[test]
    fn explicit_pem_identity_builds_without_environment_lookup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let certificate = dir.path().join("client-cert.pem");
        let private_key = dir.path().join("client-key.pem");
        crate::cert::ensure_self_signed(&certificate, &private_key).expect("identity pair");

        let config = client_config_with_identity(
            &CertTrust::SkipVerify,
            &TlsClientIdentity::PemFiles {
                certificate,
                private_key,
            },
            Some(b"phux-test/1"),
        )
        .expect("explicit PEM identity");
        assert_eq!(config.alpn_protocols, vec![b"phux-test/1".to_vec()]);
    }
}
