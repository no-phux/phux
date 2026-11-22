//! Leaf-certificate pinning: how a tunnel or a consumer trusts the relay.
//!
//! The relay serves one self-signed leaf; a client trusts exactly that leaf,
//! named by its SHA-256 fingerprint ([`fingerprint`]). No CA, no name check:
//! the relay is reached by address and its consumers name a route, not the
//! relay, in the server name.

use std::fmt::Write as _;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};

/// SHA-256 of a DER certificate as uppercase colon-separated hex
/// (`AB:CD:…`), the spelling a relay prints and a client pins.
#[must_use]
pub fn fingerprint(cert: &CertificateDer<'_>) -> String {
    let digest = Sha256::digest(cert.as_ref());
    let mut hex = String::with_capacity(digest.len() * 3);
    for (i, byte) in digest.iter().enumerate() {
        if i > 0 {
            hex.push(':');
        }
        let _ = write!(hex, "{byte:02X}");
    }
    hex
}

/// A fingerprint in any spelling (colons, spaces, case) as bare uppercase
/// hex, so pins compare equal however they were pasted.
fn normalize(fingerprint: &str) -> String {
    fingerprint
        .chars()
        .filter(char::is_ascii_hexdigit)
        .flat_map(char::to_uppercase)
        .collect()
}

/// A TLS 1.3 client config that trusts only the leaf whose fingerprint is
/// `pin`, offering `alpn`, with no client certificate.
///
/// # Errors
///
/// rustls refused the protocol versions (it does not, for TLS 1.3 on ring).
pub fn client_config(pin: &str, alpn: &[u8]) -> Result<rustls::ClientConfig, rustls::Error> {
    let mut config = stream_client_config(pin)?;
    config.alpn_protocols = vec![alpn.to_vec()];
    Ok(config)
}

/// [`client_config`] offering no ALPN: for a TLS session run inside a
/// bridged stream, whose protocol the stream already fixed.
///
/// # Errors
///
/// As [`client_config`].
pub fn stream_client_config(pin: &str) -> Result<rustls::ClientConfig, rustls::Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PinnedLeaf {
        pin: normalize(pin),
        provider: Arc::clone(&provider),
    });
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(config)
}

/// Accepts exactly the pinned leaf; signatures are still verified, so only
/// the holder of its key completes the handshake.
#[derive(Debug)]
struct PinnedLeaf {
    /// Bare uppercase hex.
    pin: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for PinnedLeaf {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let presented = fingerprint(end_entity);
        if normalize(&presented) != self.pin {
            return Err(rustls::Error::General(format!(
                "relay certificate fingerprint mismatch (got {presented})"
            )));
        }
        Ok(ServerCertVerified::assertion())
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
mod tests {
    use super::*;

    #[test]
    fn pins_compare_in_any_spelling() {
        assert_eq!(normalize("ab:CD 0f"), "ABCD0F");
        let cert = CertificateDer::from(vec![1, 2, 3]);
        let printed = fingerprint(&cert);
        assert_eq!(printed.len(), 32 * 3 - 1);
        assert_eq!(normalize(&printed), normalize(&printed.to_lowercase()));
    }

    #[test]
    fn only_the_pinned_leaf_verifies() {
        let ours = CertificateDer::from(vec![1, 2, 3]);
        let theirs = CertificateDer::from(vec![4, 5, 6]);
        let verifier = PinnedLeaf {
            pin: normalize(&fingerprint(&ours)),
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        };
        let name = ServerName::try_from("route").unwrap();
        let now = UnixTime::now();
        assert!(
            verifier
                .verify_server_cert(&ours, &[], &name, &[], now)
                .is_ok()
        );
        assert!(
            verifier
                .verify_server_cert(&theirs, &[], &name, &[], now)
                .is_err()
        );
    }
}
