//! The generated certificate must name the address phux advertises
//! (ADR-0091). Proven by a real handshake: the production acceptor on a
//! loopback socket, dialed by a rustls client whose verifier skips only
//! trust-anchor chaining (a self-signed leaf is trusted out of band) and
//! performs the real webpki name check. The TLS server name is independent of
//! the TCP destination, so claiming a CGNAT address over loopback exercises
//! the real overlay mismatch.

use std::sync::Arc;

use phux_server::transport::tls::{acceptor_from_pem, ensure_self_signed, ensure_self_signed_for};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::ParsedCertificate;
use rustls::{DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;

/// Inside Tailscale's CGNAT range, the shape `phux pair` embeds in its link.
const ROUTABLE: &str = "100.64.0.2";

/// Trusts the certificate it is shown but still checks the name, like
/// `curl --cacert remote-cert.pem`. (phux's own consumers pin the leaf hash
/// and ignore the name.)
#[derive(Debug)]
struct NameValidating(Arc<CryptoProvider>);

impl ServerCertVerifier for NameValidating {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        let parsed = ParsedCertificate::try_from(end_entity)?;
        rustls::client::verify_server_name(&parsed, server_name)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
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
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// The name-validating client's verdict on the production acceptor at
/// `cert`/`key` when it claims `server_name` over loopback.
async fn handshake_as(
    cert: &std::path::Path,
    key: &std::path::Path,
    server_name: &'static str,
) -> Result<(), String> {
    let acceptor = acceptor_from_pem(cert, key).expect("build acceptor");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");

    // The accept side fails when the client alerts; only the client's verdict counts.
    let server = tokio::spawn(async move {
        if let Ok((tcp, _)) = listener.accept().await {
            let _ = acceptor.accept(tcp).await;
        }
    });

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("client protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NameValidating(provider)))
        .with_no_client_auth();

    let tcp = TcpStream::connect(addr).await.expect("connect");
    let name = ServerName::try_from(server_name).expect("server name");
    let result = TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map(|_| ())
        .map_err(|err| err.to_string());

    server.await.expect("server task");
    result
}

/// The loopback-only certificate is refused for a routable name; naming the
/// advertised address fixes that without dropping the loopback identities or
/// covering other addresses; and `covers_name` (what `phux pair`, the listener
/// warning, and `phux doctor` report from) agrees with the handshake.
#[tokio::test]
async fn cert_names_exactly_the_advertised_and_loopback_addresses() {
    use phux_server::transport::tls::covers_name;

    let dir = tempfile::tempdir().unwrap();
    let narrow = (
        dir.path().join("narrow-cert.pem"),
        dir.path().join("narrow-key.pem"),
    );
    ensure_self_signed(&narrow.0, &narrow.1).unwrap();
    let wide = (
        dir.path().join("wide-cert.pem"),
        dir.path().join("wide-key.pem"),
    );
    ensure_self_signed_for(&wide.0, &wide.1, &[ROUTABLE.to_owned()]).unwrap();

    let cases = [
        (&narrow, "127.0.0.1", true),
        (&narrow, "localhost", true),
        (&narrow, ROUTABLE, false),
        (&wide, "127.0.0.1", true),
        (&wide, "localhost", true),
        (&wide, ROUTABLE, true),
        (&wide, "100.64.0.3", false),
    ];
    for ((cert, key), name, expected) in cases {
        let handshake = handshake_as(cert, key, name).await;
        assert_eq!(
            handshake.is_ok(),
            expected,
            "{name} against {}: {handshake:?}",
            cert.display()
        );
        if let Err(err) = handshake {
            assert!(
                err.to_lowercase().contains("name"),
                "a name rejection: {err}"
            );
        }
        let reported = covers_name(cert, name).expect("read certificate");
        assert_eq!(reported, expected, "covers_name({name})");
    }
}
