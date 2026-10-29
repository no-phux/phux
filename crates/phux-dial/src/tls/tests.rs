use super::*;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerifier};
use rustls::pki_types::{ServerName, UnixTime};

use super::verify::{leaf_fingerprint, normalize_fingerprint};

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
            policy: pin.map_or(Policy::Skip, |p| Policy::Leaf {
                pin: normalize_fingerprint(&p),
            }),
            cert_requested: None,
            refusal: AuthorityRefusal::default(),
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

#[test]
fn require_paired_needs_a_whole_identity() {
    let path = |name: &str| Some(PathBuf::from(name));
    assert_eq!(
        identity_from(None, None, false).expect("none"),
        TlsClientIdentity::None
    );
    assert!(matches!(
        identity_from(path("c"), path("k"), false),
        Ok(TlsClientIdentity::PemFiles { .. })
    ));
    assert!(matches!(
        identity_from(path("c"), path("k"), true),
        Ok(TlsClientIdentity::RequirePaired { .. })
    ));
    let bare = identity_from(None, None, true).expect_err("nothing to present");
    assert!(bare.to_string().contains(REQUIRE_PAIRED_ENV), "{bare}");
    for (cert, key) in [(path("c"), None), (None, path("k"))] {
        for require in [false, true] {
            assert!(identity_from(cert.clone(), key.clone(), require).is_err());
        }
    }
}

/// A server-side client verifier that asks for a certificate and accepts
/// any: the test cares whether the request was made, not who answered.
#[derive(Debug)]
struct AskAnyClient(Arc<rustls::crypto::CryptoProvider>);

impl rustls::server::danger::ClientCertVerifier for AskAnyClient {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        Ok(rustls::server::danger::ClientCertVerified::assertion())
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
            &self.0.signature_verification_algorithms,
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
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// One TLS 1.3 handshake over an in-memory pipe: the server asks for a
/// client certificate when `server_asks`; the client dials with
/// `identity`. The client's handshake result.
async fn handshake(server_asks: bool, identity: &TlsClientIdentity) -> Result<(), String> {
    let dir = tempfile::tempdir().expect("tempdir");
    let (cert, key) = (dir.path().join("server.pem"), dir.path().join("server.key"));
    crate::cert::ensure_self_signed(&cert, &key).expect("server pair");
    let fingerprint = crate::cert::cert_fingerprint(&cert).expect("fingerprint");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("server versions");
    let builder = if server_asks {
        builder.with_client_cert_verifier(Arc::new(AskAnyClient(provider)))
    } else {
        builder.with_no_client_auth()
    };
    let server = builder
        .with_single_cert(
            crate::cert::load_certs(&cert).expect("certs"),
            crate::cert::load_key(&key).expect("key"),
        )
        .expect("server config");
    let client = client_config_with_identity(&CertTrust::Pinned(fingerprint), identity, None)
        .expect("client config");

    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
    let name = ServerName::try_from("localhost").expect("name");
    let (dialed, _accepted) = tokio::join!(
        connector.connect(name, client_io),
        acceptor.accept(server_io)
    );
    dialed.map(drop).map_err(|err| err.to_string())
}

fn client_pair(dir: &Path) -> (PathBuf, PathBuf) {
    let (certificate, private_key) = (dir.join("client.pem"), dir.join("client.key"));
    crate::cert::ensure_self_signed(&certificate, &private_key).expect("client pair");
    (certificate, private_key)
}

/// workload-auth §3: a client that requires paired authority refuses a
/// server that did not request its certificate, inside the handshake,
/// and continues with one that did. Without the requirement, today's
/// clients keep connecting to either.
#[tokio::test]
async fn require_paired_refuses_a_server_that_did_not_ask_for_the_certificate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (certificate, private_key) = client_pair(dir.path());
    let required = TlsClientIdentity::RequirePaired {
        certificate: certificate.clone(),
        private_key: private_key.clone(),
    };
    let presented = TlsClientIdentity::PemFiles {
        certificate,
        private_key,
    };

    let downgraded = handshake(false, &required)
        .await
        .expect_err("a server that did not ask is a downgrade");
    assert!(downgraded.contains(DOWNGRADE_REFUSED), "{downgraded}");
    handshake(true, &required)
        .await
        .expect("a server that asks is paired");

    for server_asks in [false, true] {
        handshake(server_asks, &presented)
            .await
            .expect("an unrequired identity connects either way");
        handshake(server_asks, &TlsClientIdentity::None)
            .await
            .expect("no identity connects either way");
    }
}

/// A CA and a leaf it issued for server authentication, as rcgen
/// certificates, plus the leaf's key.
struct Issued {
    chain: Vec<CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
}

fn authority() -> (
    rcgen::Issuer<'static, rcgen::KeyPair>,
    CertificateDer<'static>,
) {
    let mut params =
        rcgen::CertificateParams::new(vec!["phux-workload-ca".to_owned()]).expect("params");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let key = rcgen::KeyPair::generate().expect("key");
    let cert = params.self_signed(&key).expect("ca");
    let der = cert.der().clone();
    (rcgen::Issuer::new(params, key), der)
}

fn issue(issuer: &rcgen::Issuer<'static, rcgen::KeyPair>, ca: &CertificateDer<'static>) -> Issued {
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).expect("params");
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let key = rcgen::KeyPair::generate().expect("key");
    let leaf = params.signed_by(&key, issuer).expect("leaf");
    Issued {
        chain: vec![leaf.der().clone(), ca.clone()],
        key: rustls::pki_types::PrivateKeyDer::try_from(key.serialize_der()).expect("der"),
    }
}

/// A self-signed leaf, as a server provisioned before ADR-0153 holds.
fn self_signed_leaf() -> Issued {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).expect("leaf");
    Issued {
        chain: vec![certified.cert.der().clone()],
        key: rustls::pki_types::PrivateKeyDer::try_from(certified.signing_key.serialize_der())
            .expect("der"),
    }
}

/// One TLS 1.3 handshake against a server presenting `presented`. The
/// client's result, with the authority change its verifier reported.
async fn handshake_with(
    presented: &Issued,
    trust: &CertTrust,
) -> Result<(), Option<AuthorityChange>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(presented.chain.clone(), presented.key.clone_key())
        .expect("server config");
    let (client, refusal) =
        client_config_reporting(trust, Some(&TlsClientIdentity::None), None).expect("client");
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
    let name = ServerName::try_from("localhost").expect("name");
    let (dialed, _accepted) = tokio::join!(
        connector.connect(name, client_io),
        acceptor.accept(server_io)
    );
    dialed.map(drop).map_err(|_| {
        authority_refusal(&refusal).map(|error| match error {
            DialError::AuthorityChanged(change) => change,
            other => panic!("unexpected {other}"),
        })
    })
}

fn pin_of(cert: &CertificateDer<'_>) -> String {
    leaf_fingerprint(cert)
}

/// ADR-0153: a client pinning the CA accepts every leaf it issued, so a
/// re-issued server certificate needs no re-pair.
#[tokio::test]
async fn an_authority_pin_accepts_any_leaf_the_authority_issued() {
    let (issuer, ca) = authority();
    let trust = CertTrust::Authority {
        ca: authority_fingerprint(&ca),
    };
    for _ in 0..2 {
        handshake_with(&issue(&issuer, &ca), &trust)
            .await
            .expect("a leaf the pinned authority issued");
    }
}

/// Adversarial: a server whose leaf another CA issued is refused with an
/// authority change naming both fingerprints (the authority moved, so its
/// client certificates did too).
#[tokio::test]
async fn a_swapped_authority_is_refused_naming_both_fingerprints() {
    let (issuer, ca) = authority();
    let (rogue_issuer, rogue_ca) = authority();
    let original = issue(&issuer, &ca);
    let swapped = issue(&rogue_issuer, &rogue_ca);
    let change = handshake_with(
        &swapped,
        &CertTrust::Authority {
            ca: authority_fingerprint(&ca),
        },
    )
    .await
    .expect_err("another authority")
    .expect("reported as an authority change");
    assert_eq!(change.pinned, authority_fingerprint(&ca));
    assert_eq!(change.presented, Some(authority_fingerprint(&rogue_ca)));
    let message = change.to_string();
    assert!(
        message.contains(&authority_fingerprint(&ca))
            && message.contains(&authority_fingerprint(&rogue_ca))
            && message.contains("certificate authority changed"),
        "{message}"
    );
    handshake_with(
        &original,
        &CertTrust::Authority {
            ca: authority_fingerprint(&ca)
                .to_uppercase()
                .replace("SHA256:", "sha256:"),
        },
    )
    .await
    .expect("positive control, in any hex case");
}

/// Adversarial: a forged chain that names the pinned CA but whose leaf
/// the CA never signed is not the pinned authority, and a self-signed
/// leaf is refused (ADR-0179 retired the leaf pinned beside the CA).
#[tokio::test]
async fn a_forged_chain_or_a_self_signed_leaf_is_refused() {
    let (_, ca) = authority();
    let legacy = self_signed_leaf();
    let forged = Issued {
        chain: vec![legacy.chain[0].clone(), ca.clone()],
        key: legacy.key.clone_key(),
    };
    let pinned = CertTrust::Authority {
        ca: authority_fingerprint(&ca),
    };
    for presented in [&forged, &legacy] {
        let change = handshake_with(presented, &pinned)
            .await
            .expect_err("no chain to the pinned authority")
            .expect("an authority change");
        assert_eq!(change.presented, None, "the leaf chains to nothing");
    }
}

/// A leaf pin trusts its leaf and learns nothing from it, whatever chain
/// it arrives with.
#[tokio::test]
async fn a_leaf_pin_holds_its_leaf_only() {
    let (issuer, ca) = authority();
    let genuine = issue(&issuer, &ca);
    let pinned = CertTrust::Pinned(pin_of(&genuine.chain[0]));
    handshake_with(&genuine, &pinned)
        .await
        .expect("pinned leaf");
    let other = issue(&issuer, &ca);
    assert!(
        handshake_with(&other, &pinned).await.is_err(),
        "another leaf of the same authority is not the pinned leaf"
    );
}

#[test]
fn pins_select_the_authority_then_the_leaf() {
    assert_eq!(CertTrust::from_pins(None, None), None);
    assert_eq!(
        CertTrust::from_pins(Some("AB".to_owned()), None),
        Some(CertTrust::Pinned("AB".to_owned()))
    );
    assert_eq!(
        CertTrust::from_pins(Some("AB".to_owned()), Some("sha256:cd".to_owned())),
        Some(CertTrust::Authority {
            ca: "sha256:cd".to_owned(),
        })
    );
    assert_eq!(
        CertTrust::from_pins(Some(" ".to_owned()), Some(String::new())),
        None,
        "blank pins pin nothing"
    );
}

/// Requiring paired authority is refused against skip-verify, and turns
/// off session resumption.
#[test]
fn require_paired_disables_session_resumption() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (certificate, private_key) = client_pair(dir.path());
    let required = TlsClientIdentity::RequirePaired {
        certificate,
        private_key,
    };
    let unpinned = client_config_with_identity(&CertTrust::SkipVerify, &required, None)
        .expect_err("an unpinned server could send the request itself");
    assert!(unpinned.to_string().contains("pinned"), "{unpinned}");
    let config = client_config_with_identity(&CertTrust::Pinned("00".repeat(32)), &required, None)
        .expect("config");
    assert_eq!(
        format!("{:?}", config.resumption),
        format!("{:?}", rustls::client::Resumption::disabled()),
    );
}
