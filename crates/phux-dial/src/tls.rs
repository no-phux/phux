//! Shared TLS trust policy for remote dial transports.
//!
//! QUIC and secure WebSocket both use the same operator flow: `phux pair`
//! prints a self-signed certificate fingerprint, and the dialer pins that
//! fingerprint for routable hosts. Loopback dev may skip certificate
//! verification while still exercising the encrypted transport.

use std::fmt::Write as _;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::DialError;

/// How a remote dialer decides to trust the server's TLS certificate.
///
/// TLS still provides encryption in both modes. The choice here is whether the
/// server's self-signed certificate is pinned out-of-band, or accepted blindly
/// for loopback-only development.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertTrust {
    /// Accept the server's certificate without verification. **Loopback dev
    /// only**.
    SkipVerify,
    /// Pin the server's leaf-certificate SHA-256 fingerprint, in the
    /// colon-or-bare hex shape `phux pair` prints.
    Pinned(String),
}

/// Build a rustls client config with the phux remote trust policy.
///
/// `alpn` is transport-specific: QUIC needs `phux-quic/1`; WebSocket leaves it
/// unset because the RFC 6455 upgrade happens at HTTP level.
///
/// Public so that anything dialing a phux listener gets *this* verifier rather
/// than a hand-rolled copy of it — including the listener tests on the server
/// side, which previously carried their own `ServerCertVerifier`. A test that
/// re-implements the trust policy is a test that can quietly disagree with it.
pub fn client_config(
    trust: &CertTrust,
    alpn: Option<&[u8]>,
) -> Result<rustls::ClientConfig, DialError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier: Arc<dyn rustls::client::danger::ServerCertVerifier> = match trust {
        CertTrust::SkipVerify => Arc::new(SkipServerVerification(provider.clone())),
        CertTrust::Pinned(fingerprint) => Arc::new(PinnedFingerprint {
            provider: provider.clone(),
            expected: normalize_fingerprint(fingerprint),
        }),
    };

    let mut crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|err| DialError::Connect(format!("build TLS client config: {err}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    if let Some(alpn) = alpn {
        crypto.alpn_protocols = vec![alpn.to_vec()];
    }
    Ok(crypto)
}

/// Uppercase hex digits only — drops separators and whitespace so a pin pasted
/// as `AB:CD:...`, `abcd...`, or with spaces compares the same way.
fn normalize_fingerprint(fingerprint: &str) -> String {
    fingerprint
        .chars()
        .filter(char::is_ascii_hexdigit)
        .flat_map(char::to_uppercase)
        .collect()
}

/// SHA-256 of a leaf certificate as bare uppercase hex (no separators).
fn leaf_fingerprint(cert: &rustls::pki_types::CertificateDer<'_>) -> String {
    let digest = Sha256::digest(cert.as_ref());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02X}");
    }
    hex
}

#[derive(Debug)]
struct PinnedFingerprint {
    provider: Arc<rustls::crypto::CryptoProvider>,
    expected: String,
}

impl rustls::client::danger::ServerCertVerifier for PinnedFingerprint {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let actual = leaf_fingerprint(end_entity);
        if actual == self.expected {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "server certificate fingerprint mismatch (pinned {}, got {})",
                self.expected, actual
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
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
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
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

#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn normalize_fingerprint_is_separator_and_case_insensitive() {
        let colons = "ab:CD:12:Ef";
        let bare = "ABCD12EF";
        assert_eq!(normalize_fingerprint(colons), bare);
        assert_eq!(normalize_fingerprint(bare), bare);
        assert_eq!(
            normalize_fingerprint("  ab cd 12 ef  "),
            bare,
            "whitespace is dropped too"
        );
    }
}
