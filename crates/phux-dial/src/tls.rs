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
//!
//! A server whose certificate the workload CA issued presents the CA after
//! its leaf (ADR-0153). A client that pins that CA ([`CertTrust::Authority`])
//! accepts any leaf the CA issued, and refuses a server presenting another
//! authority with [`AuthorityChange`], never silently trusting it. A client
//! that still pins only the leaf learns the CA the pinned leaf chains to on
//! its first successful handshake ([`CertTrust::PinnedLearning`]).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rustls::client::ResolvesClientCert;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};

use crate::DialError;

/// How a remote dialer decides to trust the server's TLS certificate.
///
/// Every mode encrypts and verifies handshake signatures; they differ in what
/// the presented chain is checked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertTrust {
    /// Accept the server's certificate without verification. **Loopback dev
    /// only**.
    SkipVerify,
    /// Pin the server's leaf-certificate SHA-256 fingerprint, in the
    /// colon-or-bare hex shape `phux pair` prints.
    Pinned(String),
    /// [`Self::Pinned`], and hand `learn` the `sha256:` fingerprint of the
    /// certificate authority the pinned leaf was issued by, when the server
    /// presents one ([`chain_authority`]). The pinned leaf authenticates the
    /// channel the authority arrives on, so recording it is trust on first
    /// connect, not on first sight (ADR-0153).
    PinnedLearning {
        /// The leaf pin, as for [`Self::Pinned`].
        leaf: String,
        /// Called with the presented authority after the leaf matched.
        learn: AuthorityLearner,
    },
    /// Pin the server's certificate authority (ADR-0153): accept a leaf the
    /// authority issued, or exactly the `leaf` pinned beside it (a server
    /// whose certificate predates the authority). A server presenting any
    /// other authority, or neither, is refused with an [`AuthorityChange`].
    Authority {
        /// `sha256:` fingerprint of the DER CA certificate (any hex case or
        /// separators are accepted).
        ca: String,
        /// The leaf pin kept beside it, as for [`Self::Pinned`].
        leaf: Option<String>,
    },
}

impl CertTrust {
    /// The trust a client's stored pins select: the authority when one is
    /// pinned, else the leaf (learning the authority when `learn` is given),
    /// else `None`, which the caller resolves (loopback skip, or refusal).
    #[must_use]
    pub fn from_pins(
        leaf: Option<String>,
        ca: Option<String>,
        learn: Option<AuthorityLearner>,
    ) -> Option<Self> {
        let leaf = leaf.filter(|pin| !pin.trim().is_empty());
        if let Some(ca) = ca.filter(|pin| !pin.trim().is_empty()) {
            return Some(Self::Authority { ca, leaf });
        }
        let leaf = leaf?;
        Some(match learn {
            Some(learn) => Self::PinnedLearning { leaf, learn },
            None => Self::Pinned(leaf),
        })
    }
}

/// Receives the certificate authority a leaf-pinned server presented.
///
/// See [`CertTrust::PinnedLearning`]. It runs inside the TLS handshake, so it
/// should be quick; it is called on every handshake that presents one.
#[derive(Clone)]
pub struct AuthorityLearner(Arc<dyn Fn(&str) + Send + Sync>);

impl AuthorityLearner {
    /// Wrap `learn`, which receives the canonical `sha256:` fingerprint.
    pub fn new(learn: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self(Arc::new(learn))
    }

    /// Hand it `authority`, as the verifier does after a pinned leaf.
    pub fn learn(&self, authority: &str) {
        (self.0)(authority);
    }
}

impl std::fmt::Debug for AuthorityLearner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthorityLearner(..)")
    }
}

/// Two learners are equal when they are the same callback.
impl PartialEq for AuthorityLearner {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for AuthorityLearner {}

/// A server that did not present the pinned certificate authority
/// (`workload-auth.md` §2 `authority_changed`): the handshake was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityChange {
    /// The pinned authority, canonical `sha256:` form.
    pub pinned: String,
    /// The authority the server presented, or `None` when it presented none
    /// its certificate chains to and its leaf matched no pin.
    pub presented: Option<String>,
}

impl std::fmt::Display for AuthorityChange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.presented {
            Some(presented) => write!(
                f,
                "the server's certificate authority changed: pinned {}, presented {presented}",
                self.pinned
            ),
            None => write!(
                f,
                "the server's certificate authority changed: pinned {}, presented none its certificate chains to",
                self.pinned
            ),
        }
    }
}

/// Where a handshake's verifier leaves an [`AuthorityChange`] for its dialer,
/// which reports it as [`DialError::AuthorityChanged`] instead of a bare TLS
/// failure.
pub(crate) type AuthorityRefusal = Arc<Mutex<Option<AuthorityChange>>>;

/// The [`DialError::AuthorityChanged`] a failed handshake's verifier left,
/// if it left one.
pub(crate) fn authority_refusal(refusal: &AuthorityRefusal) -> Option<DialError> {
    refusal
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
        .map(DialError::AuthorityChanged)
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
    client_config_reporting(trust, None, alpn).map(|(config, _)| config)
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
    client_config_reporting(trust, Some(identity), alpn).map(|(config, _)| config)
}

/// [`client_config`] or, with `identity`, [`client_config_with_identity`],
/// plus the slot the verifier fills when it refuses a changed authority.
pub(crate) fn client_config_reporting(
    trust: &CertTrust,
    identity: Option<&TlsClientIdentity>,
    alpn: Option<&[u8]>,
) -> Result<(rustls::ClientConfig, AuthorityRefusal), DialError> {
    let from_env;
    let identity = if let Some(identity) = identity {
        identity
    } else {
        from_env = client_identity_from_env()?;
        &from_env
    };
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
    let refusal = AuthorityRefusal::default();
    let verifier = Arc::new(Verifier {
        provider: provider.clone(),
        policy: Policy::from(trust),
        cert_requested: cert_requested.clone(),
        refusal: Arc::clone(&refusal),
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
    Ok((crypto, refusal))
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

/// Prefix of the canonical authority fingerprint (`workload-auth.md` §2).
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
enum Policy {
    Skip,
    Leaf {
        pin: String,
        learn: Option<AuthorityLearner>,
    },
    Authority {
        ca: String,
        leaf: Option<String>,
    },
}

impl From<&CertTrust> for Policy {
    fn from(trust: &CertTrust) -> Self {
        match trust {
            CertTrust::SkipVerify => Self::Skip,
            CertTrust::Pinned(pin) => Self::Leaf {
                pin: normalize_fingerprint(pin),
                learn: None,
            },
            CertTrust::PinnedLearning { leaf, learn } => Self::Leaf {
                pin: normalize_fingerprint(leaf),
                learn: Some(learn.clone()),
            },
            CertTrust::Authority { ca, leaf } => Self::Authority {
                ca: normalize_authority(ca),
                leaf: leaf.as_deref().map(normalize_fingerprint),
            },
        }
    }
}

/// The phux server-certificate verifier: applies the [`Policy`] to the
/// presented chain and always verifies handshake signatures. With
/// `cert_requested` it first refuses a server that did not ask for the client
/// certificate.
#[derive(Debug)]
struct Verifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
    policy: Policy,
    /// `Some` for [`TlsClientIdentity::RequirePaired`]: whether this
    /// handshake's `CertificateRequest` has arrived. TLS 1.3 sends it before
    /// the server's Certificate, so it is settled when this verifier runs.
    cert_requested: Option<Arc<AtomicBool>>,
    /// Filled when [`Policy::Authority`] refuses the presented chain.
    refusal: AuthorityRefusal,
}

impl Verifier {
    /// The leaf pin, plus learning the authority the pinned leaf chains to.
    fn verify_leaf(
        pin: &str,
        learn: Option<&AuthorityLearner>,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let actual = leaf_fingerprint(end_entity);
        if actual != pin {
            return Err(rustls::Error::General(format!(
                "server certificate fingerprint mismatch (pinned {pin}, got {actual})"
            )));
        }
        // The authority comes from the leaf's own signature, which a server
        // without the authority's key cannot forge, so the pinned leaf is all
        // that makes it trustworthy.
        if let Some(learn) = learn
            && let Some(authority) = chain_authority(end_entity, intermediates, now)
        {
            learn.learn(&authority);
        }
        Ok(ServerCertVerified::assertion())
    }

    /// The authority pin: the presented authority must be the pinned one,
    /// and the leaf must chain to it or match the leaf pin kept beside it.
    fn verify_authority(
        &self,
        ca: &str,
        leaf: Option<&str>,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let presented = chain_authority(end_entity, intermediates, now);
        let pinned_presented = presented
            .as_deref()
            .is_some_and(|presented| normalize_authority(presented) == ca);
        let leaf_matches = leaf.is_some_and(|pin| leaf_fingerprint(end_entity) == pin);
        // A different authority is refused even beside a matching leaf: the
        // server's authority, and every client certificate it issued, moved.
        let accepted = match &presented {
            Some(_) => pinned_presented,
            None => leaf_matches,
        };
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
            Policy::Leaf { pin, learn } => {
                Self::verify_leaf(pin, learn.as_ref(), end_entity, intermediates, now)
            }
            Policy::Authority { ca, leaf } => {
                self.verify_authority(ca, leaf.as_deref(), end_entity, intermediates, now)
            }
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
                policy: pin.map_or(Policy::Skip, |p| Policy::Leaf {
                    pin: normalize_fingerprint(&p),
                    learn: None,
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

    fn issue(
        issuer: &rcgen::Issuer<'static, rcgen::KeyPair>,
        ca: &CertificateDer<'static>,
    ) -> Issued {
        let mut params =
            rcgen::CertificateParams::new(vec!["localhost".to_owned()]).expect("params");
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
        let certified =
            rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).expect("leaf");
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
            leaf: None,
        };
        for _ in 0..2 {
            handshake_with(&issue(&issuer, &ca), &trust)
                .await
                .expect("a leaf the pinned authority issued");
        }
    }

    /// Adversarial: a server whose leaf another CA issued is refused with an
    /// authority change naming both fingerprints, even when the pinned leaf
    /// sits beside the pin (the authority moved, so its client certificates
    /// did too).
    #[tokio::test]
    async fn a_swapped_authority_is_refused_naming_both_fingerprints() {
        let (issuer, ca) = authority();
        let (rogue_issuer, rogue_ca) = authority();
        let original = issue(&issuer, &ca);
        let swapped = issue(&rogue_issuer, &rogue_ca);
        for leaf in [None, Some(pin_of(&swapped.chain[0]))] {
            let change = handshake_with(
                &swapped,
                &CertTrust::Authority {
                    ca: authority_fingerprint(&ca),
                    leaf,
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
        }
        handshake_with(
            &original,
            &CertTrust::Authority {
                ca: authority_fingerprint(&ca)
                    .to_uppercase()
                    .replace("SHA256:", "sha256:"),
                leaf: None,
            },
        )
        .await
        .expect("positive control, in any hex case");
    }

    /// Adversarial: a forged chain that names the pinned CA but whose leaf
    /// the CA never signed is not the pinned authority, and a self-signed
    /// leaf is accepted only when it is exactly the leaf pinned beside the
    /// CA (a server that predates its authority).
    #[tokio::test]
    async fn a_forged_chain_or_stale_leaf_is_refused_and_the_pinned_leaf_continues() {
        let (_, ca) = authority();
        let legacy = self_signed_leaf();
        let forged = Issued {
            chain: vec![legacy.chain[0].clone(), ca.clone()],
            key: legacy.key.clone_key(),
        };
        let unpinned_leaf = CertTrust::Authority {
            ca: authority_fingerprint(&ca),
            leaf: None,
        };
        for presented in [&forged, &legacy] {
            let change = handshake_with(presented, &unpinned_leaf)
                .await
                .expect_err("no chain to the pinned authority")
                .expect("an authority change");
            assert_eq!(change.presented, None, "the leaf chains to nothing");
        }
        let stale = CertTrust::Authority {
            ca: authority_fingerprint(&ca),
            leaf: Some(pin_of(&self_signed_leaf().chain[0])),
        };
        assert!(
            handshake_with(&legacy, &stale).await.is_err(),
            "a stale leaf pin"
        );
        let continuing = CertTrust::Authority {
            ca: authority_fingerprint(&ca),
            leaf: Some(pin_of(&legacy.chain[0])),
        };
        handshake_with(&legacy, &continuing)
            .await
            .expect("the leaf pinned beside the authority keeps working");
    }

    /// Trust on first connect: a leaf-pinned client learns the CA its pinned
    /// leaf chains to, and never one a server without the pinned leaf, or a
    /// chain the leaf does not verify under, offers.
    #[tokio::test]
    async fn a_leaf_pin_learns_only_the_authority_its_own_leaf_chains_to() {
        let (issuer, ca) = authority();
        let (_, rogue_ca) = authority();
        let genuine = issue(&issuer, &ca);
        let learned = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorder = {
            let learned = Arc::clone(&learned);
            AuthorityLearner::new(move |authority| {
                learned.lock().unwrap().push(authority.to_owned());
            })
        };
        let pinned = |leaf: &CertificateDer<'_>| CertTrust::PinnedLearning {
            leaf: pin_of(leaf),
            learn: recorder.clone(),
        };

        handshake_with(&genuine, &pinned(&genuine.chain[0]))
            .await
            .expect("pinned leaf");
        assert_eq!(*learned.lock().unwrap(), vec![authority_fingerprint(&ca)]);

        // A server that is not the pinned one teaches nothing.
        let other = issue(&issuer, &ca);
        assert!(
            handshake_with(&other, &pinned(&genuine.chain[0]))
                .await
                .is_err()
        );
        // The genuine leaf beside a CA that never issued it teaches nothing.
        let spliced = Issued {
            chain: vec![genuine.chain[0].clone(), rogue_ca],
            key: genuine.key.clone_key(),
        };
        handshake_with(&spliced, &pinned(&genuine.chain[0]))
            .await
            .expect("leaf pin holds");
        // A self-signed leaf has no authority to learn.
        let legacy = self_signed_leaf();
        handshake_with(&legacy, &pinned(&legacy.chain[0]))
            .await
            .expect("leaf pin holds");
        assert_eq!(
            learned.lock().unwrap().len(),
            1,
            "{:?}",
            learned.lock().unwrap()
        );
    }

    #[test]
    fn pins_select_the_authority_then_the_leaf() {
        let learner = AuthorityLearner::new(|_| {});
        assert_eq!(
            CertTrust::from_pins(None, None, Some(learner.clone())),
            None
        );
        assert_eq!(
            CertTrust::from_pins(Some("AB".to_owned()), None, None),
            Some(CertTrust::Pinned("AB".to_owned()))
        );
        assert_eq!(
            CertTrust::from_pins(Some("AB".to_owned()), None, Some(learner.clone())),
            Some(CertTrust::PinnedLearning {
                leaf: "AB".to_owned(),
                learn: learner.clone(),
            })
        );
        assert_eq!(
            CertTrust::from_pins(
                Some("AB".to_owned()),
                Some("sha256:cd".to_owned()),
                Some(learner)
            ),
            Some(CertTrust::Authority {
                ca: "sha256:cd".to_owned(),
                leaf: Some("AB".to_owned()),
            })
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
        let config =
            client_config_with_identity(&CertTrust::Pinned("00".repeat(32)), &required, None)
                .expect("config");
        assert_eq!(
            format!("{:?}", config.resumption),
            format!("{:?}", rustls::client::Resumption::disabled()),
        );
    }
}
