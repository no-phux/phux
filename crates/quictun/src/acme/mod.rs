//! Public certificates for an exposed service from an ACME CA such as Let's
//! Encrypt (RFC 8555; feature `acme`).
//!
//! Validation is TLS-ALPN-01 (RFC 8737): the CA connects to the name on port
//! 443 offering the `acme-tls/1` ALPN, and the service answers with a
//! challenge certificate. A relay's TCP ingress routes that connection by
//! its server name like any other, so the service answers through the
//! tunnel: no inbound port at the service, no change to the relay, and the
//! certificate's key never leaves the service. The relay must listen on TCP
//! 443 (`--tcp-listen 0.0.0.0:443`), with the name resolving to it.
//!
//! [`AcmeCerts`] is the service's rustls certificate resolver: the current
//! certificate for ordinary clients, a challenge certificate for an
//! `acme-tls/1` hello. [`maintain`] obtains the certificate and renews it
//! before it expires.

mod http;
mod jws;

use std::collections::HashMap;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use rustls::pki_types::pem::PemObject as _;

use self::jws::{AccountKey, b64};

/// The ALPN a TLS-ALPN-01 validation offers (RFC 8737 §6.2).
pub const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

/// Let's Encrypt's production directory.
pub const LETS_ENCRYPT: &str = "https://acme-v02.api.letsencrypt.org/directory";

/// Renew once less than this share of the certificate's lifetime is left:
/// a 90-day certificate at 30 days, a 6-day one at 2 days.
const RENEW_AT_ONE_IN: u64 = 3;

/// The longest [`maintain`] waits between checks.
const MAX_CHECK: Duration = Duration::from_hours(12);

/// The shortest [`maintain`] waits between checks.
const MIN_CHECK: Duration = Duration::from_secs(60);

/// First retry after a failed issuance; doubles up to [`RETRY_CAP`]. Five
/// minutes keeps a run of failed validations under Let's Encrypt's limit of
/// five per hour for one account and name.
const RETRY_BASE: Duration = Duration::from_mins(5);

/// The longest a poll honors a CA's `Retry-After`.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);

/// Longest wait between failed issuances.
const RETRY_CAP: Duration = Duration::from_hours(6);

/// Why an ACME exchange failed.
#[derive(Debug, thiserror::Error)]
pub enum AcmeError {
    /// The connection or the HTTP exchange failed.
    #[error("acme http: {0}")]
    Http(String),
    /// The CA answered with an error, or something the client cannot use.
    #[error("acme: {0}")]
    Protocol(String),
    /// Reading or writing the account or certificate files failed.
    #[error("acme store: {0}")]
    Store(String),
    /// A key or certificate could not be made.
    #[error("acme crypto: {0}")]
    Crypto(String),
}

/// What to obtain, from whom, and where to keep it.
#[derive(Debug, Clone)]
pub struct AcmeConfig {
    /// The CA's directory URL, such as [`LETS_ENCRYPT`].
    pub directory: String,
    /// The name the certificate covers: the route's `R.<domain>` name.
    pub domain: String,
    /// An email for expiry notices from the CA, if any.
    pub contact: Option<String>,
    /// Where the account key and the certificate live.
    pub state_dir: PathBuf,
    /// The roots trusted for the CA's own HTTPS (Mozilla's, plus a test
    /// CA's when pointing at one).
    pub roots: Arc<rustls::RootCertStore>,
    /// How often to poll an authorization or order while the CA works.
    pub poll_interval: Duration,
    /// How many polls before giving up on one.
    pub poll_attempts: u32,
}

impl AcmeConfig {
    /// A config for `domain` from `directory`, trusting Mozilla's roots,
    /// polling every second for up to two minutes.
    #[must_use]
    pub fn new(directory: &str, domain: &str, state_dir: &Path) -> Self {
        Self {
            directory: directory.to_owned(),
            domain: domain.to_ascii_lowercase(),
            contact: None,
            state_dir: state_dir.to_owned(),
            roots: Arc::new(crate::forward::web_roots()),
            poll_interval: Duration::from_secs(1),
            poll_attempts: 120,
        }
    }

    fn dir(&self) -> PathBuf {
        self.state_dir.join("acme")
    }

    fn account_key_path(&self) -> PathBuf {
        self.dir().join("account.key")
    }

    fn account_url_path(&self) -> PathBuf {
        self.dir().join("account.url")
    }

    /// The key and its chain in one owner-only PEM file, so replacing them
    /// is one rename and a crash never pairs a key with another chain.
    fn bundle_path(&self) -> PathBuf {
        self.dir().join(format!("{}.pem", self.domain))
    }
}

/// The service's certificate resolver: the current certificate for
/// ordinary clients, the pending challenge certificate for an `acme-tls/1`
/// hello naming its domain.
#[derive(Default)]
pub struct AcmeCerts {
    current: RwLock<Option<Arc<rustls::sign::CertifiedKey>>>,
    challenges: Mutex<HashMap<String, Arc<rustls::sign::CertifiedKey>>>,
}

impl std::fmt::Debug for AcmeCerts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcmeCerts")
            .field("has_certificate", &self.has_certificate())
            .finish_non_exhaustive()
    }
}

impl AcmeCerts {
    /// Whether a certificate is being served yet.
    #[must_use]
    pub fn has_certificate(&self) -> bool {
        self.current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }

    fn set_current(&self, key: Arc<rustls::sign::CertifiedKey>) {
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = Some(key);
    }

    fn challenges(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, Arc<rustls::sign::CertifiedKey>>> {
        self.challenges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl rustls::server::ResolvesServerCert for AcmeCerts {
    fn resolve(
        &self,
        hello: rustls::server::ClientHello<'_>,
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        let validating = hello
            .alpn()
            .is_some_and(|mut alpns| alpns.any(|alpn| alpn == ACME_TLS_ALPN));
        if validating {
            let name = hello.server_name()?.to_ascii_lowercase();
            return self.challenges().get(&name).cloned();
        }
        self.current
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// A TLS server config that serves `certs`, offering `alpns` (the service's
/// own, such as `http/1.1`) and `acme-tls/1` for validation.
///
/// A client offering ALPNs none of which the service lists is refused, so a
/// web service should list `http/1.1`.
#[must_use]
pub fn service_config(certs: Arc<AcmeCerts>, alpns: Vec<Vec<u8>>) -> rustls::ServerConfig {
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap_or_else(|_| unreachable!("ring supports the safe default versions"))
    .with_no_client_auth()
    .with_cert_resolver(certs);
    config.alpn_protocols = alpns;
    config.alpn_protocols.push(ACME_TLS_ALPN.to_vec());
    config
}

/// Keep `certs` serving a valid certificate for `config.domain`, forever.
///
/// Load the stored one, obtain one when there is none or less than a third
/// of its lifetime is left, and check again in proportion (at most every 12
/// hours). An issuance waits until `reachable` reads `true`, since the CA
/// validates through the relay: [`crate::forward::expose`] sets it while
/// its tunnel is up. Failures are logged and retried with backoff (5
/// minutes, doubling to 6 hours).
pub async fn maintain(
    config: AcmeConfig,
    certs: Arc<AcmeCerts>,
    mut reachable: tokio::sync::watch::Receiver<bool>,
) {
    if let Some(stored) = load_stored(&config) {
        certs.set_current(stored.key);
    }
    let mut retry = RETRY_BASE;
    loop {
        if let Some(wait) = next_check(&config) {
            tokio::time::sleep(wait).await;
            continue;
        }
        if reachable.wait_for(|up| *up).await.is_err() {
            return;
        }
        let wait = match issue(&config, &certs).await {
            Ok(()) => {
                tracing::info!(domain = %config.domain, "acme certificate issued");
                retry = RETRY_BASE;
                continue;
            }
            Err(err) => {
                tracing::warn!(domain = %config.domain, %err, retry_in = ?retry, "acme issuance failed");
                retry
            }
        };
        retry = (retry * 2).min(RETRY_CAP);
        tokio::time::sleep(wait).await;
    }
}

/// How long until the stored certificate needs renewing, clamped to
/// [`MIN_CHECK`]..[`MAX_CHECK`]; `None` when it needs it now (or there is
/// none).
fn next_check(config: &AcmeConfig) -> Option<Duration> {
    let stored = load_stored(config)?;
    let lifetime = stored.not_after.saturating_sub(stored.not_before);
    let renew_at = stored.not_after.saturating_sub(lifetime / RENEW_AT_ONE_IN);
    let until = renew_at.checked_sub(unix_now()).filter(|secs| *secs > 0)?;
    Some(Duration::from_secs(until).clamp(MIN_CHECK, MAX_CHECK))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// A stored certificate that loads and matches its key.
struct Stored {
    key: Arc<rustls::sign::CertifiedKey>,
    /// The leaf's validity, in Unix seconds.
    not_before: u64,
    not_after: u64,
}

fn load_stored(config: &AcmeConfig) -> Option<Stored> {
    let bundle = config.bundle_path();
    if !bundle.exists() {
        return None;
    }
    let certs = crate::cert::load_certs(&bundle).ok()?;
    let (not_before, not_after) = validity(certs.first()?)?;
    let key = crate::cert::load_key(&bundle).ok()?;
    let key = Arc::new(certified_key(certs, key).ok()?);
    Some(Stored {
        key,
        not_before,
        not_after,
    })
}

/// A leaf's notBefore and notAfter, in Unix seconds.
fn validity(cert: &rustls::pki_types::CertificateDer<'_>) -> Option<(u64, u64)> {
    let (_, parsed) = x509_parser::parse_x509_certificate(cert.as_ref()).ok()?;
    let validity = parsed.validity();
    Some((
        u64::try_from(validity.not_before.timestamp()).ok()?,
        u64::try_from(validity.not_after.timestamp()).ok()?,
    ))
}

fn certified_key(
    certs: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> Result<rustls::sign::CertifiedKey, AcmeError> {
    let provider = rustls::crypto::ring::default_provider();
    let signing = provider
        .key_provider
        .load_private_key(key)
        .map_err(|err| AcmeError::Crypto(err.to_string()))?;
    let certified = rustls::sign::CertifiedKey::new(certs, signing);
    certified
        .keys_match()
        .map_err(|err| AcmeError::Crypto(format!("certificate and key differ: {err}")))?;
    Ok(certified)
}

/// Write `bytes` to `path`, owner-only, through a temporary file and a
/// rename so a crash never leaves half a secret.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), AcmeError> {
    let store = |err: std::io::Error| AcmeError::Store(format!("{}: {err}", path.display()));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(store)?;
    }
    let temp = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temp)
        .map_err(store)?;
    file.write_all(bytes).map_err(store)?;
    file.sync_all().map_err(store)?;
    std::fs::rename(&temp, path).map_err(store)
}

/// One ACME session: the directory, the account, and the next nonce.
struct Session<'a> {
    config: &'a AcmeConfig,
    key: AccountKey,
    directory: Value,
    nonce: Option<String>,
    kid: Option<String>,
}

/// Obtain a certificate for `config.domain` and start serving it.
///
/// # Errors
///
/// The CA refused, validation failed, or the files could not be written.
pub async fn issue(config: &AcmeConfig, certs: &AcmeCerts) -> Result<(), AcmeError> {
    let mut session = Session::open(config).await?;
    session.account().await?;
    let (order_url, order) = match session.new_order().await {
        // The stored account is gone (a CA that forgot it): register again.
        Err(AcmeError::Protocol(problem)) if problem.contains("accountDoesNotExist") => {
            session.kid = None;
            session.account().await?;
            session.new_order().await?
        }
        ordered => ordered?,
    };
    for authorization in order["authorizations"].as_array().into_iter().flatten() {
        let url = authorization
            .as_str()
            .ok_or_else(|| AcmeError::Protocol("authorization is not a URL".to_owned()))?;
        session.authorize(url, certs).await?;
    }
    let finalize = url_field(&order, "finalize")?;
    let (chain_pem, key_pem) = session.finalize(&order_url, &finalize).await?;
    let chain = rustls::pki_types::CertificateDer::pem_slice_iter(chain_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| AcmeError::Protocol(format!("certificate chain: {err}")))?;
    let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
        .map_err(|err| AcmeError::Crypto(err.to_string()))?;
    let certified = certified_key(chain, key)?;
    write_private(
        &config.bundle_path(),
        format!("{key_pem}{chain_pem}").as_bytes(),
    )?;
    certs.set_current(Arc::new(certified));
    Ok(())
}

/// A string URL member of an ACME object.
fn url_field(object: &Value, name: &str) -> Result<String, AcmeError> {
    object[name]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| AcmeError::Protocol(format!("no {name} URL in {object}")))
}

/// A string member of an ACME object, or `""`.
fn text<'v>(object: &'v Value, name: &str) -> &'v str {
    object[name].as_str().unwrap_or_default()
}

/// Removes a pending challenge certificate when validation ends, however it
/// ends.
struct ChallengeGuard<'a> {
    certs: &'a AcmeCerts,
    domain: String,
}

impl Drop for ChallengeGuard<'_> {
    fn drop(&mut self) {
        self.certs.challenges().remove(&self.domain);
    }
}

impl<'a> Session<'a> {
    async fn open(config: &'a AcmeConfig) -> Result<Self, AcmeError> {
        let (key, created) = AccountKey::load_or_create(&config.account_key_path())?;
        // A stored account URL names the account of the stored key; a new
        // key has no account yet, whatever the old URL says.
        let kid = if created {
            let _ = std::fs::remove_file(config.account_url_path());
            None
        } else {
            std::fs::read_to_string(config.account_url_path())
                .ok()
                .map(|url| url.trim().to_owned())
                .filter(|url| !url.is_empty())
        };
        let response =
            http::request(&config.roots, "GET", &config.directory, None, http::JSON).await?;
        if response.status != 200 {
            return Err(AcmeError::Protocol(format!(
                "directory {}: status {}",
                config.directory, response.status
            )));
        }
        Ok(Self {
            config,
            key,
            directory: response.json()?,
            nonce: None,
            kid,
        })
    }

    /// The next anti-replay nonce: the one the last response carried, or a
    /// fresh one from `newNonce`.
    async fn nonce(&mut self) -> Result<String, AcmeError> {
        if let Some(nonce) = self.nonce.take() {
            return Ok(nonce);
        }
        let url = url_field(&self.directory, "newNonce")?;
        let response = http::request(&self.config.roots, "GET", &url, None, http::JSON).await?;
        response
            .header("replay-nonce")
            .map(str::to_owned)
            .ok_or_else(|| AcmeError::Protocol("newNonce sent no Replay-Nonce".to_owned()))
    }

    /// A signed POST (a POST-as-GET without a payload). A `badNonce`
    /// refusal is retried once with a fresh nonce, as RFC 8555 §6.5 asks.
    async fn post(
        &mut self,
        url: &str,
        payload: Option<&Value>,
    ) -> Result<http::Response, AcmeError> {
        self.post_accepting(url, payload, http::JSON).await
    }

    /// [`Self::post`], asking for `accept` (a certificate chain is PEM).
    async fn post_accepting(
        &mut self,
        url: &str,
        payload: Option<&Value>,
        accept: &str,
    ) -> Result<http::Response, AcmeError> {
        for attempt in 0..2 {
            let nonce = self.nonce().await?;
            let body = self.key.sign(url, &nonce, self.kid.as_deref(), payload)?;
            let response =
                http::request(&self.config.roots, "POST", url, Some(&body), accept).await?;
            self.nonce = response.header("replay-nonce").map(str::to_owned);
            if response.status < 400 {
                return Ok(response);
            }
            let problem = response.json().unwrap_or(Value::Null);
            if attempt == 0 && text(&problem, "type").ends_with(":badNonce") {
                continue;
            }
            return Err(AcmeError::Protocol(format!(
                "{url}: status {}: {} {}",
                response.status,
                text(&problem, "type"),
                text(&problem, "detail"),
            )));
        }
        Err(AcmeError::Protocol(format!("{url}: nonces kept failing")))
    }

    /// Register the account, unless one is already stored.
    async fn account(&mut self) -> Result<(), AcmeError> {
        if self.kid.is_some() {
            return Ok(());
        }
        let mut payload = json!({ "termsOfServiceAgreed": true });
        if let Some(contact) = &self.config.contact {
            payload["contact"] = json!([format!("mailto:{contact}")]);
        }
        let url = url_field(&self.directory, "newAccount")?;
        let response = self.post(&url, Some(&payload)).await?;
        let kid = response
            .header("location")
            .ok_or_else(|| AcmeError::Protocol("newAccount sent no Location".to_owned()))?
            .to_owned();
        write_private(&self.config.account_url_path(), kid.as_bytes())?;
        self.kid = Some(kid);
        Ok(())
    }

    /// A new order for the domain: its URL and its body.
    async fn new_order(&mut self) -> Result<(String, Value), AcmeError> {
        let payload = json!({
            "identifiers": [{ "type": "dns", "value": self.config.domain }],
        });
        let url = url_field(&self.directory, "newOrder")?;
        let response = self.post(&url, Some(&payload)).await?;
        let order_url = response
            .header("location")
            .ok_or_else(|| AcmeError::Protocol("newOrder sent no Location".to_owned()))?
            .to_owned();
        Ok((order_url, response.json()?))
    }

    /// Satisfy one authorization by TLS-ALPN-01: serve the challenge
    /// certificate, tell the CA to validate, and wait for its verdict.
    async fn authorize(&mut self, url: &str, certs: &AcmeCerts) -> Result<(), AcmeError> {
        let authorization = self.post(url, None).await?.json()?;
        if text(&authorization, "status") == "valid" {
            return Ok(());
        }
        let challenge = authorization["challenges"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|challenge| text(challenge, "type") == "tls-alpn-01")
            .ok_or_else(|| {
                AcmeError::Protocol("the CA offered no tls-alpn-01 challenge".to_owned())
            })?;
        let challenge_url = url_field(challenge, "url")?;
        let key_authorization = format!("{}.{}", text(challenge, "token"), self.key.thumbprint());
        let domain = self.config.domain.clone();
        let certified = challenge_certificate(&domain, &key_authorization)?;
        certs
            .challenges()
            .insert(domain.clone(), Arc::new(certified));
        let _guard = ChallengeGuard { certs, domain };
        self.post(&challenge_url, Some(&json!({}))).await?;
        let authorization = self.poll(url, &["pending", "processing"]).await?;
        match text(&authorization, "status") {
            "valid" => Ok(()),
            status => Err(AcmeError::Protocol(format!(
                "authorization {status}: {}",
                authorization["challenges"]
            ))),
        }
    }

    /// Finalize the order with a fresh key's CSR and download the chain.
    /// Returns the chain and the key, both PEM.
    async fn finalize(
        &mut self,
        order_url: &str,
        finalize_url: &str,
    ) -> Result<(String, String), AcmeError> {
        let crypto = |err: rcgen::Error| AcmeError::Crypto(err.to_string());
        let key = rcgen::KeyPair::generate().map_err(crypto)?;
        let mut params =
            rcgen::CertificateParams::new(vec![self.config.domain.clone()]).map_err(crypto)?;
        // No subject: rcgen's default common name ("rcgen self signed
        // cert") is not a name in the order, and a CA that reads the CN as
        // one (Boulder does) refuses the CSR. The SAN carries the domain.
        params.distinguished_name = rcgen::DistinguishedName::new();
        let csr = params.serialize_request(&key).map_err(crypto)?;
        self.post(finalize_url, Some(&json!({ "csr": b64(csr.der()) })))
            .await?;
        let order = self
            .poll(order_url, &["pending", "ready", "processing"])
            .await?;
        if text(&order, "status") != "valid" {
            return Err(AcmeError::Protocol(format!(
                "order {}: {order}",
                text(&order, "status")
            )));
        }
        let certificate_url = url_field(&order, "certificate")?;
        let chain = self
            .post_accepting(&certificate_url, None, "application/pem-certificate-chain")
            .await?;
        let chain = String::from_utf8(chain.body)
            .map_err(|_| AcmeError::Protocol("certificate chain is not text".to_owned()))?;
        Ok((chain, key.serialize_pem()))
    }

    /// POST-as-GET `url` until its status leaves `waiting`, or give up.
    async fn poll(&mut self, url: &str, waiting: &[&str]) -> Result<Value, AcmeError> {
        for _ in 0..self.config.poll_attempts {
            let response = self.post(url, None).await?;
            let object = response.json()?;
            if !waiting.contains(&text(&object, "status")) {
                return Ok(object);
            }
            // Honor the CA's Retry-After (in seconds), within reason.
            let asked = response
                .header("retry-after")
                .and_then(|secs| secs.trim().parse().ok())
                .map_or(Duration::ZERO, Duration::from_secs);
            tokio::time::sleep(asked.min(MAX_RETRY_AFTER).max(self.config.poll_interval)).await;
        }
        Err(AcmeError::Protocol(format!(
            "{url}: still pending after polling"
        )))
    }
}

/// The TLS-ALPN-01 certificate for `domain` (RFC 8737 §3): self-signed,
/// naming the domain, with the critical acmeIdentifier extension holding the
/// SHA-256 of the key authorization.
fn challenge_certificate(
    domain: &str,
    key_authorization: &str,
) -> Result<rustls::sign::CertifiedKey, AcmeError> {
    let crypto = |err: rcgen::Error| AcmeError::Crypto(err.to_string());
    let digest = Sha256::digest(key_authorization.as_bytes());
    let mut params = rcgen::CertificateParams::new(vec![domain.to_owned()]).map_err(crypto)?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.custom_extensions = vec![rcgen::CustomExtension::new_acme_identifier(&digest)];
    let key = rcgen::KeyPair::generate().map_err(crypto)?;
    let cert = params.self_signed(&key).map_err(crypto)?;
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into());
    // Built here from this very key, so no `keys_match` check: it parses the
    // certificate with webpki, which refuses the critical acmeIdentifier
    // extension RFC 8737 requires.
    let signing = rustls::crypto::ring::default_provider()
        .key_provider
        .load_private_key(key_der)
        .map_err(|err| AcmeError::Crypto(err.to_string()))?;
    Ok(rustls::sign::CertifiedKey::new(
        vec![cert.der().clone()],
        signing,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_challenge_certificate_carries_the_key_authorization_digest() {
        let certified = challenge_certificate("laptop.tunnel.test", "token.thumb").unwrap();
        let (_, parsed) = x509_parser::parse_x509_certificate(certified.cert[0].as_ref()).unwrap();
        let oid = x509_parser::oid_registry::Oid::from(&[1, 3, 6, 1, 5, 5, 7, 1, 31]).unwrap();
        let extension = parsed
            .extensions()
            .iter()
            .find(|extension| extension.oid == oid)
            .expect("acmeIdentifier");
        assert!(extension.critical, "RFC 8737 §3: critical");
        let mut expected = vec![0x04, 0x20];
        expected.extend_from_slice(&Sha256::digest(b"token.thumb"));
        assert_eq!(
            extension.value,
            expected.as_slice(),
            "an OCTET STRING of the digest"
        );
        let names = parsed.subject_alternative_name().unwrap().unwrap();
        assert!(format!("{:?}", names.value).contains("laptop.tunnel.test"));
    }

    #[test]
    fn the_resolver_serves_challenges_only_to_acme_tls_hellos() {
        // Covered end to end in tests/acme.rs, where real hellos arrive;
        // here, the bookkeeping: a guard removes its challenge.
        let certs = AcmeCerts::default();
        let certified = Arc::new(challenge_certificate("a.test", "t.k").unwrap());
        certs.challenges().insert("a.test".to_owned(), certified);
        drop(ChallengeGuard {
            certs: &certs,
            domain: "a.test".to_owned(),
        });
        assert!(certs.challenges().is_empty());
        assert!(!certs.has_certificate());
    }

    /// Store a self-signed certificate for `config.domain` valid from
    /// `issued_ago` until `left` from now.
    fn store(config: &AcmeConfig, issued_ago: Duration, left: Duration) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec![config.domain.clone()]).unwrap();
        let now = std::time::SystemTime::now();
        params.not_before = (now - issued_ago).into();
        params.not_after = (now + left).into();
        let cert = params.self_signed(&key).unwrap();
        let bundle = format!("{}{}", key.serialize_pem(), cert.pem());
        write_private(&config.bundle_path(), bundle.as_bytes()).unwrap();
    }

    #[test]
    fn renewal_comes_at_a_third_of_the_lifetime_whatever_its_length() {
        let day = Duration::from_hours(24);
        let dir = tempfile::tempdir().unwrap();
        let config = AcmeConfig::new("https://ca.test/dir", "a.test", dir.path());
        assert!(next_check(&config).is_none(), "nothing stored: issue now");

        // 90 days: due at 30 left.
        store(&config, day * 50, day * 40);
        assert!(load_stored(&config).is_some(), "the bundle loads");
        let wait = next_check(&config).expect("not due with 40 of 90 days left");
        assert_eq!(wait, MAX_CHECK, "checked again within 12 hours");
        store(&config, day * 65, day * 25);
        assert!(next_check(&config).is_none(), "due with 25 of 90 days left");

        // 6 days: due at 2 left, never immediately after issuance.
        store(&config, Duration::ZERO, day * 6);
        let wait = next_check(&config).expect("a fresh 6-day certificate is not due");
        assert_eq!(wait, MAX_CHECK);
        store(&config, day * 5, day);
        assert!(next_check(&config).is_none(), "due with 1 of 6 days left");

        // One hour: checks follow it closely, never more often than a minute.
        store(&config, Duration::ZERO, Duration::from_secs(3600));
        let wait = next_check(&config).unwrap();
        assert!(
            wait <= Duration::from_mins(40) && wait >= MIN_CHECK,
            "{wait:?}"
        );
    }

    /// A new account key forgets the old account's URL.
    #[tokio::test]
    async fn a_new_account_key_does_not_reuse_a_stale_account_url() {
        let dir = tempfile::tempdir().unwrap();
        let config = AcmeConfig::new("https://127.0.0.1:1/dir", "a.test", dir.path());
        write_private(&config.account_url_path(), b"https://ca.test/acct/old").unwrap();
        // The directory is unreachable, but the key is created first.
        assert!(Session::open(&config).await.is_err());
        assert!(!config.account_url_path().exists(), "stale URL removed");
    }
}
