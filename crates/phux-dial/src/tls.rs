//! Shared TLS trust policy for remote dial transports.
//!
//! `phux pair` prints a self-signed certificate fingerprint and the dialer
//! pins it for routable hosts; loopback dev may skip verification while still
//! encrypting.
//!
//! A client that requires paired authority ([`REQUIRE_PAIRED_ENV`], or
//! [`TlsClientIdentity::RequirePaired`]) refuses a server that does not
//! request its certificate (`docs/spec/workload-auth.md` §3): the handshake
//! fails before any phux byte is sent, so nothing stateful rides a channel
//! the server did not authenticate the client on.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustls::client::ResolvesClientCert;
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
    /// [`Self::PemFiles`], and refuse a server that does not request the
    /// client certificate: that is a downgrade, not permission to continue
    /// (`docs/spec/workload-auth.md` §3). Session resumption is disabled so
    /// every handshake shows whether the server asked.
    ///
    /// A config built with this identity serves one connection: the evidence
    /// that the server asked lives in the config, not in the connection.
    RequirePaired {
        /// PEM certificate chain, leaf first.
        certificate: PathBuf,
        /// PEM private key corresponding to the leaf certificate.
        private_key: PathBuf,
    },
}

/// Turns on [`TlsClientIdentity::RequirePaired`] for environment-read dials.
///
/// Its presence is the switch, like the server's `PHUX_WORKLOAD_MTLS`.
pub const REQUIRE_PAIRED_ENV: &str = "PHUX_WORKLOAD_REQUIRE_PAIRED";

/// The handshake refusal for a server that did not request the client
/// certificate (workload-auth §3 downgrade).
pub const DOWNGRADE_REFUSED: &str = "the server did not request a client certificate; \
     refusing to continue without paired authority";

/// Build a rustls client config with the phux remote trust policy, taking the
/// optional workload identity from `PHUX_WORKLOAD_CERT` / `PHUX_WORKLOAD_KEY`.
///
/// [`REQUIRE_PAIRED_ENV`] makes the identity [`TlsClientIdentity::RequirePaired`].
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
    if matches!(identity, TlsClientIdentity::RequirePaired { .. })
        && matches!(trust, CertTrust::SkipVerify)
    {
        // An unpinned server could send the CertificateRequest itself.
        return Err(DialError::Connect(
            "requiring paired authority needs a pinned server certificate, not skip-verify"
                .to_owned(),
        ));
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let pem = match identity {
        TlsClientIdentity::None => None,
        TlsClientIdentity::PemFiles {
            certificate,
            private_key,
        }
        | TlsClientIdentity::RequirePaired {
            certificate,
            private_key,
        } => Some(load_pem_identity(certificate, private_key)?),
    };
    // Set by the certificate resolver, which rustls consults only when the
    // server sends a CertificateRequest; read by the server-certificate
    // verifier, which TLS 1.3 runs after it.
    let cert_requested = matches!(identity, TlsClientIdentity::RequirePaired { .. })
        .then(|| Arc::new(AtomicBool::new(false)));
    let verifier = Arc::new(Verifier {
        provider: provider.clone(),
        pin: match trust {
            CertTrust::SkipVerify => None,
            CertTrust::Pinned(fingerprint) => Some(normalize_fingerprint(fingerprint)),
        },
        cert_requested: cert_requested.clone(),
    });

    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|err| DialError::Connect(format!("build TLS client config: {err}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    let mut crypto = match pem {
        Some((certs, key)) => builder
            .with_client_auth_cert(certs, key)
            .map_err(|err| DialError::Connect(format!("build workload identity: {err}")))?,
        None => builder.with_no_client_auth(),
    };
    if let Some(requested) = cert_requested {
        crypto.client_auth_cert_resolver = Arc::new(NoteCertRequest {
            inner: Arc::clone(&crypto.client_auth_cert_resolver),
            requested,
        });
        // A resumed TLS 1.3 session carries no CertificateRequest and runs no
        // certificate verifier, so it could not show the server asked.
        crypto.resumption = rustls::client::Resumption::disabled();
    }
    if let Some(alpn) = alpn {
        crypto.alpn_protocols = vec![alpn.to_vec()];
    }
    Ok(crypto)
}

/// A PEM certificate chain and its private key.
type PemIdentity = (
    Vec<CertificateDer<'static>>,
    rustls::pki_types::PrivateKeyDer<'static>,
);

/// One PEM certificate chain and its private key, read from explicit paths.
fn load_pem_identity(certificate: &Path, private_key: &Path) -> Result<PemIdentity, DialError> {
    let certs = CertificateDer::pem_file_iter(certificate)
        .map_err(|err| DialError::Connect(format!("read workload certificate: {err}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| DialError::Connect(format!("read workload certificate: {err}")))?;
    let key = rustls::pki_types::PrivateKeyDer::from_pem_file(private_key)
        .map_err(|err| DialError::Connect(format!("read workload key: {err}")))?;
    Ok((certs, key))
}

/// Wraps the client-certificate resolver to record that the server asked:
/// rustls calls `resolve` only on a `CertificateRequest`.
#[derive(Debug)]
struct NoteCertRequest {
    inner: Arc<dyn ResolvesClientCert>,
    requested: Arc<AtomicBool>,
}

impl ResolvesClientCert for NoteCertRequest {
    fn resolve(
        &self,
        root_hint_subjects: &[&[u8]],
        sigschemes: &[rustls::SignatureScheme],
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        self.requested.store(true, Ordering::SeqCst);
        self.inner.resolve(root_hint_subjects, sigschemes)
    }

    fn only_raw_public_keys(&self) -> bool {
        self.inner.only_raw_public_keys()
    }

    fn has_certs(&self) -> bool {
        self.inner.has_certs()
    }
}

/// Optional client identity for paired mTLS endpoints. Both variables are
/// required; a half-configured identity fails rather than silently dialing
/// unauthenticated.
fn client_identity_from_env() -> Result<TlsClientIdentity, DialError> {
    identity_from(
        std::env::var_os("PHUX_WORKLOAD_CERT").map(PathBuf::from),
        std::env::var_os("PHUX_WORKLOAD_KEY").map(PathBuf::from),
        std::env::var_os(REQUIRE_PAIRED_ENV).is_some(),
    )
}

/// The identity the three variables name. Requiring paired authority with no
/// certificate to present fails: no server could satisfy it.
fn identity_from(
    cert: Option<PathBuf>,
    key: Option<PathBuf>,
    require_paired: bool,
) -> Result<TlsClientIdentity, DialError> {
    match (cert, key, require_paired) {
        (None, None, false) => Ok(TlsClientIdentity::None),
        (None, None, true) => Err(DialError::Connect(format!(
            "{REQUIRE_PAIRED_ENV} needs a workload identity: set PHUX_WORKLOAD_CERT and PHUX_WORKLOAD_KEY"
        ))),
        (Some(certificate), Some(private_key), false) => Ok(TlsClientIdentity::PemFiles {
            certificate,
            private_key,
        }),
        (Some(certificate), Some(private_key), true) => Ok(TlsClientIdentity::RequirePaired {
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
/// With `cert_requested` it first refuses a server that did not ask for the
/// client certificate.
#[derive(Debug)]
struct Verifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
    /// Normalized expected fingerprint; `None` is [`CertTrust::SkipVerify`].
    pin: Option<String>,
    /// `Some` for [`TlsClientIdentity::RequirePaired`]: whether this
    /// handshake's `CertificateRequest` has arrived. TLS 1.3 sends it before
    /// the server's Certificate, so it is settled when this verifier runs.
    cert_requested: Option<Arc<AtomicBool>>,
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
        if let Some(requested) = &self.cert_requested
            && !requested.swap(false, Ordering::SeqCst)
        {
            return Err(rustls::Error::General(DOWNGRADE_REFUSED.to_owned()));
        }
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
                cert_requested: None,
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
        let config =
            client_config_with_identity(&CertTrust::Pinned("00".repeat(32)), &required, None)
                .expect("config");
        assert_eq!(
            format!("{:?}", config.resumption),
            format!("{:?}", rustls::client::Resumption::disabled()),
        );
    }
}
