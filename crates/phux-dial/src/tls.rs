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
//! accepts any leaf the CA issued, and refuses any other chain with
//! [`AuthorityChange`], never silently trusting it. Nothing is learned from a
//! connection: a pin is made at pairing or not at all (ADR-0179).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rustls::client::ResolvesClientCert;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;

use crate::DialError;

mod verify;
use verify::{Policy, Verifier};
pub use verify::{authority_fingerprint, chain_authority};

/// How a remote dialer decides to trust the server's TLS certificate.
///
/// Every mode encrypts and verifies handshake signatures; they differ in what
/// the presented chain is checked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertTrust {
    /// Accept the server's certificate without verification. **Loopback dev
    /// only**.
    SkipVerify,
    /// Pin the endpoint's leaf-certificate SHA-256 fingerprint, in the
    /// colon-or-bare hex shape `phux pair` prints: a relay, a federation
    /// satellite, or a server whose certificate no workload CA issued.
    Pinned(String),
    /// Pin the server's certificate authority (ADR-0153): accept a leaf the
    /// authority issued. A server presenting any other authority, or none,
    /// is refused with an [`AuthorityChange`].
    Authority {
        /// `sha256:` fingerprint of the DER CA certificate (any hex case or
        /// separators are accepted).
        ca: String,
    },
}

impl CertTrust {
    /// The trust a client's stored pins select: the authority when one is
    /// pinned, else the leaf, else `None`, which the caller resolves
    /// (loopback skip, or refusal). A leaf pinned beside an authority is not
    /// consulted: the authority decides (ADR-0179).
    #[must_use]
    pub fn from_pins(leaf: Option<String>, ca: Option<String>) -> Option<Self> {
        if let Some(ca) = ca.filter(|pin| !pin.trim().is_empty()) {
            return Some(Self::Authority { ca });
        }
        leaf.filter(|pin| !pin.trim().is_empty()).map(Self::Pinned)
    }
}

/// A server that did not present the pinned certificate authority
/// (`workload-auth.md` §2 `authority_changed`): the handshake was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityChange {
    /// The pinned authority, canonical `sha256:` form.
    pub pinned: String,
    /// The authority the server presented, or `None` when its certificate
    /// chains to none.
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
    /// A certificate chain whose private key the client cannot read: a
    /// platform keystore signs each handshake through [`HeldIdentity`]'s
    /// signer (ADR-0154). Never read from the environment.
    Held(HeldIdentity),
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

/// A client certificate chain and a signer for its key, which stays where it
/// was generated (a Secure Enclave, a StrongBox): rustls asks the signer for
/// every handshake signature.
#[derive(Clone)]
pub struct HeldIdentity {
    /// The issued chain, leaf first.
    pub chain: Vec<CertificateDer<'static>>,
    /// Signs for the leaf's key.
    pub key: Arc<dyn rustls::sign::SigningKey>,
}

impl std::fmt::Debug for HeldIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldIdentity")
            .field("chain", &self.chain.len())
            .finish_non_exhaustive()
    }
}

/// Two held identities are equal when they are the same chain and signer.
impl PartialEq for HeldIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.chain == other.chain && Arc::ptr_eq(&self.key, &other.key)
    }
}

impl Eq for HeldIdentity {}

/// Always presents one held identity.
#[derive(Debug)]
struct PresentHeld(Arc<rustls::sign::CertifiedKey>);

impl ResolvesClientCert for PresentHeld {
    fn resolve(
        &self,
        _root_hint_subjects: &[&[u8]],
        _sigschemes: &[rustls::SignatureScheme],
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(Arc::clone(&self.0))
    }

    fn has_certs(&self) -> bool {
        true
    }
}

/// Turns on [`TlsClientIdentity::RequirePaired`] for environment-read dials.
///
/// Its presence is the switch.
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
        TlsClientIdentity::None | TlsClientIdentity::Held(_) => None,
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
    let mut crypto = match (pem, identity) {
        (Some((certs, key)), _) => builder
            .with_client_auth_cert(certs, key)
            .map_err(|err| DialError::Connect(format!("build workload identity: {err}")))?,
        (None, TlsClientIdentity::Held(held)) => {
            builder.with_client_cert_resolver(Arc::new(PresentHeld(Arc::new(
                rustls::sign::CertifiedKey::new(held.chain.clone(), Arc::clone(&held.key)),
            ))))
        }
        (None, _) => builder.with_no_client_auth(),
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

/// Optional client identity for paired mTLS endpoints, from the environment.
///
/// Both variables are required; a half-configured identity fails rather than
/// silently dialing unauthenticated. Public so a relayed dial, whose
/// end-to-end session takes an explicit identity, presents the same one a
/// direct dial would.
///
/// # Errors
///
/// [`DialError::Connect`] for a half-configured identity, or one
/// `PHUX_REQUIRE_PAIRED` requires and the environment does not name.
pub fn client_identity_from_env() -> Result<TlsClientIdentity, DialError> {
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
