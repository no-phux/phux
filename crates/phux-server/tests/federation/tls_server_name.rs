//! The generated certificate must name the address phux advertises
//! (phux-q9a0, ADR-0091).
//!
//! `phux pair` prints a connect link pointing a device at a routable overlay
//! address, and the certificate whose fingerprint it prints alongside used to
//! carry `DNS:localhost, IP:127.0.0.1, IP:::1` and nothing else. Against a
//! live server that reproduced as:
//!
//! ```text
//! $ openssl s_client -connect 100.79.155.27:8787 -verify_ip 100.79.155.27
//! verify error:num=64:IP address mismatch
//! ```
//!
//! ## Why this is a handshake and not a SAN-list assertion
//!
//! Reading the SANs back out and comparing strings tests the code that wrote
//! them against itself. The property that matters is one level further out —
//! *a client that validates the server name completes the handshake* — and
//! nothing but a handshake proves it. So these tests stand up the production
//! [`acceptor_from_pem`] on a real TCP socket and dial it with a real rustls
//! client whose verifier performs the real webpki name check.
//!
//! The verifier skips exactly one thing: chaining to a trust anchor. A
//! self-signed leaf has no issuer to chain to, and the operator supplies that
//! trust out of band — importing `remote-cert.pem` into a trust store, or
//! passing it as `curl --cacert`. Name validation is the step that fails
//! *after* that trust is granted, so it is the step under test. Signature
//! verification is left fully intact.
//!
//! ## Why the client dials 127.0.0.1 while asking for 100.64.0.2
//!
//! The TLS server name is independent of the TCP destination: rustls verifies
//! the name the caller asked for against the certificate it is shown. Claiming
//! a CGNAT address (the range Tailscale assigns from, ADR-0037) over a loopback
//! socket exercises the exact mismatch a real overlay dial hits, with no
//! network, no tailnet, and no nondeterminism.
//!
//! [`acceptor_from_pem`]: phux_server::transport::tls::acceptor_from_pem

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]

use std::sync::Arc;

use phux_server::transport::tls::{acceptor_from_pem, ensure_self_signed, ensure_self_signed_for};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::ParsedCertificate;
use rustls::{DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;

/// A routable stand-in for a real overlay address: inside Tailscale's CGNAT
/// range (`100.64.0.0/10`), so it is exactly the shape `phux pair` embeds in
/// its link, and unroutable from this test either way.
const ROUTABLE: &str = "100.64.0.2";

/// A client that trusts the certificate it is shown but still checks the name.
///
/// This is the operator who imported `remote-cert.pem` into a trust store, or
/// the `curl --cacert remote-cert.pem` invocation. It is deliberately NOT how
/// phux's own consumers behave: `phux-dial`'s `CertTrust::Pinned` compares the
/// leaf SHA-256 and ignores the server name entirely, as does phux-mobile's
/// verifier, so neither is affected by SAN coverage in either direction.
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

/// Terminate one TLS connection with the production acceptor at `cert`/`key`,
/// while a name-validating client dials it claiming to be `server_name`.
///
/// Returns the client's handshake result. The TCP destination is always
/// loopback; only the claimed name varies.
async fn handshake_as(
    cert: &std::path::Path,
    key: &std::path::Path,
    server_name: &'static str,
) -> Result<(), String> {
    let acceptor = acceptor_from_pem(cert, key).expect("build acceptor");
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local addr");

    // The server half is allowed to fail: when the client rejects the name it
    // sends a fatal alert, and the accept side sees that as an error. Only the
    // client's verdict is asserted on.
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

/// The defect, pinned: the address-agnostic certificate does not name a
/// routable address, and a name-validating client refuses it.
///
/// This is the assertion that was true of every certificate phux generated
/// before phux-q9a0, including for the address `phux pair` was printing.
#[tokio::test]
async fn loopback_only_cert_is_refused_for_a_routable_name() {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("remote-cert.pem");
    let key = dir.path().join("remote-key.pem");
    ensure_self_signed(&cert, &key).unwrap();

    let err = handshake_as(&cert, &key, ROUTABLE)
        .await
        .expect_err("a certificate that does not name the address must be refused");
    assert!(
        err.to_lowercase().contains("name"),
        "expected a server-name rejection, got: {err}"
    );

    // Same certificate, loopback name: accepted. So the refusal above is
    // about the name and not about anything else in the handshake.
    handshake_as(&cert, &key, "127.0.0.1")
        .await
        .expect("loopback must still verify");
}

/// The fix: naming the advertised address at generation time makes the same
/// client accept the same routable name.
#[tokio::test]
async fn cert_naming_the_advertised_address_verifies_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("remote-cert.pem");
    let key = dir.path().join("remote-key.pem");
    ensure_self_signed_for(&cert, &key, &[ROUTABLE.to_owned()]).unwrap();

    handshake_as(&cert, &key, ROUTABLE)
        .await
        .expect("the advertised address must verify");

    // The loopback identities are still there — the dev path is not traded
    // away for the remote one.
    handshake_as(&cert, &key, "127.0.0.1")
        .await
        .expect("127.0.0.1 must still verify");
    handshake_as(&cert, &key, "localhost")
        .await
        .expect("localhost must still verify");

    // And a *different* routable address is still correctly refused, so the
    // certificate names one address rather than waving everything through.
    handshake_as(&cert, &key, "100.64.0.3")
        .await
        .expect_err("an address the certificate does not name must still be refused");
}

/// `covers_name` must agree with the handshake, because it is what `phux pair`,
/// the listener warning, and `phux doctor` all report from. A reporting helper
/// that disagreed with the wire would be worse than no helper.
#[tokio::test]
async fn covers_name_agrees_with_the_handshake() {
    use phux_server::transport::tls::covers_name;

    let dir = tempfile::tempdir().unwrap();
    let narrow_cert = dir.path().join("narrow-cert.pem");
    let narrow_key = dir.path().join("narrow-key.pem");
    ensure_self_signed(&narrow_cert, &narrow_key).unwrap();

    let wide_cert = dir.path().join("wide-cert.pem");
    let wide_key = dir.path().join("wide-key.pem");
    ensure_self_signed_for(&wide_cert, &wide_key, &[ROUTABLE.to_owned()]).unwrap();

    for (cert, key) in [(&narrow_cert, &narrow_key), (&wide_cert, &wide_key)] {
        for name in ["127.0.0.1", "localhost", ROUTABLE, "100.64.0.3"] {
            let reported = covers_name(cert, name).expect("read certificate");
            let handshake = handshake_as(cert, key, name).await.is_ok();
            assert_eq!(
                reported,
                handshake,
                "covers_name said {reported} for {name} against {}, handshake said {handshake}",
                cert.display()
            );
        }
    }
}
