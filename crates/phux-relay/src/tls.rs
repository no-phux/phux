//! TLS termination for the relay's single QUIC endpoint.
//!
//! One endpoint advertises BOTH ALPNs — the dedicated connector protocol
//! (`phux-relay/1`) and the production consumer protocol (`phux-quic/1`);
//! which leg a connection belongs to is read back from the negotiated ALPN,
//! never from the byte stream (ADR-0051 invariant 7).
//!
//! Consumer routing is TLS SNI (ADR-0052 Decision 1): a consumer hello
//! whose server name is absent or not an enrolled route is refused **at
//! the TLS layer** by `SniGate`, a `ResolvesServerCert` that declines to
//! produce a certificate — the handshake aborts and zero phux-shaped bytes
//! are ever exchanged. Connector hellos (which offer the relay ALPN) pass
//! the gate; their authentication is the stream-0 token preamble.
//!
//! Certificate provisioning is modeled on `phux-server`'s
//! `transport::tls`: a persisted self-signed pair, no-op when both files
//! exist so the pinned fingerprint stays stable across restarts.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use phux_protocol::policy::{QUIC_ALPN, QUIC_RELAY_ALPN};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};

use crate::RelayError;
use crate::tokens::RouteTokenStore;

/// Default persisted path for the relay's self-signed certificate:
/// `<state-dir>/relay-cert.pem`, sibling of the server's
/// `remote-cert.pem`.
#[must_use]
pub fn default_relay_cert_path() -> PathBuf {
    crate::paths::state_dir().join("relay-cert.pem")
}

/// Default persisted path for the relay's private key:
/// `<state-dir>/relay-key.pem`.
#[must_use]
pub fn default_relay_key_path() -> PathBuf {
    crate::paths::state_dir().join("relay-key.pem")
}

/// Provision a self-signed certificate + key at the given paths if either
/// is missing.
///
/// A complete pair is left untouched, so the fingerprint both legs pin
/// stays stable across restarts (and operator-supplied certificates work
/// for free). The certificate is public; the key is written owner-only
/// (`0o600`). SANs cover loopback names — irrelevant to fingerprint
/// pinning, but they keep a conventionally-validating client working.
pub fn ensure_self_signed(cert_path: &Path, key_path: &Path) -> Result<(), RelayError> {
    if cert_path.exists() && key_path.exists() {
        return Ok(());
    }
    let sans = vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
    ];
    let certified = rcgen::generate_simple_self_signed(sans)?;
    if let Some(parent) = cert_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(cert_path, certified.cert.pem())?;
    let mut key_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(key_path)?;
    io::Write::write_all(&mut key_file, certified.key_pair.serialize_pem().as_bytes())?;
    Ok(())
}

/// SHA-256 fingerprint of the leaf certificate, formatted as uppercase
/// colon-separated hex (`AB:CD:...`) — identical to the shape
/// `phux pair` prints, so relay pins read the same everywhere.
pub fn cert_fingerprint(cert_path: &Path) -> Result<String, RelayError> {
    let certs = load_certs(cert_path)?;
    let leaf = certs
        .first()
        .ok_or_else(|| RelayError::NoCerts(cert_path.display().to_string()))?;
    let digest = Sha256::digest(leaf.as_ref());
    let hex: Vec<String> = digest.iter().map(|b| format!("{b:02X}")).collect();
    Ok(hex.join(":"))
}

/// Build the relay's rustls `ServerConfig`: TLS 1.3 only, no client auth,
/// both ALPNs, and the [`SniGate`] certificate resolver enforcing
/// enrolled-route SNI for consumer legs.
pub(crate) fn server_config(
    cert_path: &Path,
    key_path: &Path,
    tokens_path: &Path,
) -> Result<rustls::ServerConfig, RelayError> {
    let certs = load_certs(cert_path)?;
    let key = PrivateKeyDer::from_pem_file(key_path)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let signing_key = provider.key_provider.load_private_key(key)?;
    let certified = Arc::new(rustls::sign::CertifiedKey::new(certs, signing_key));

    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(RelayError::Rustls)?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(SniGate {
            key: certified,
            tokens_path: tokens_path.to_owned(),
        }));
    config.alpn_protocols = vec![QUIC_RELAY_ALPN.to_vec(), QUIC_ALPN.to_vec()];
    Ok(config)
}

/// TLS-layer SNI refusal (ADR-0052 Decision 1).
///
/// Returning `None` from `resolve` aborts the handshake before any
/// application byte can flow — an unknown or absent route name never
/// reaches phux code, indistinguishable from a non-phux TLS server.
///
/// The enrolled-route set is re-read from the token store per handshake
/// (the same per-attempt re-read as tunnel auth), so `phux relay pair`
/// takes effect on a running relay without a restart. An unreadable store
/// fails closed: consumers are refused, never waved through.
pub(crate) struct SniGate {
    /// The relay's one certified key, served to every admitted hello.
    key: Arc<rustls::sign::CertifiedKey>,
    /// Route-token store path; source of the enrolled-route set.
    tokens_path: PathBuf,
}

/// Redacted: the certified key never appears in logs.
impl std::fmt::Debug for SniGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SniGate")
            .field("tokens_path", &self.tokens_path)
            .finish_non_exhaustive()
    }
}

impl rustls::server::ResolvesServerCert for SniGate {
    fn resolve(
        &self,
        client_hello: rustls::server::ClientHello<'_>,
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        let offers_relay_alpn = client_hello
            .alpn()
            .is_some_and(|mut alpns| alpns.any(|alpn| alpn == QUIC_RELAY_ALPN));
        let routes = if offers_relay_alpn {
            BTreeSet::new()
        } else {
            enrolled_routes(&self.tokens_path)
        };
        let sni = client_hello.server_name();
        if gate_allows(offers_relay_alpn, sni, &routes) {
            Some(Arc::clone(&self.key))
        } else {
            tracing::debug!(
                sni = sni.unwrap_or("<absent>"),
                "refused at TLS: unknown or absent SNI"
            );
            None
        }
    }
}

/// The gate's decision, as a pure function.
///
/// A hello offering the relay ALPN is a connector leg: SNI is not
/// load-bearing there (authentication is the stream-0 token preamble), so
/// it always passes. Anything else is a consumer leg and must name an
/// enrolled route via SNI; absent or unknown names are refused.
pub(crate) fn gate_allows(
    offers_relay_alpn: bool,
    sni: Option<&str>,
    routes: &BTreeSet<String>,
) -> bool {
    offers_relay_alpn || sni.is_some_and(|name| routes.contains(name))
}

/// The enrolled-route set, re-read from the token store. Unreadable or
/// malformed stores yield the empty set (fail closed).
pub(crate) fn enrolled_routes(tokens_path: &Path) -> BTreeSet<String> {
    match RouteTokenStore::load(tokens_path) {
        Ok(store) => store.routes(),
        Err(err) => {
            tracing::warn!(%err, "route-token store unreadable; refusing all consumers");
            BTreeSet::new()
        }
    }
}

/// Read the PEM certificate chain.
fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, RelayError> {
    let certs = CertificateDer::pem_file_iter(path)?.collect::<Result<Vec<_>, _>>()?;
    if certs.is_empty() {
        return Err(RelayError::NoCerts(path.display().to_string()));
    }
    Ok(certs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn routes(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|&n| n.to_owned()).collect()
    }

    #[test]
    fn gate_admits_relay_alpn_regardless_of_sni() {
        let enrolled = routes(&["alpha"]);
        assert!(gate_allows(true, None, &enrolled));
        assert!(gate_allows(true, Some("not-enrolled"), &enrolled));
        assert!(gate_allows(true, Some("alpha"), &BTreeSet::new()));
    }

    #[test]
    fn gate_admits_consumers_only_for_enrolled_sni() {
        let enrolled = routes(&["alpha", "beta"]);
        assert!(gate_allows(false, Some("alpha"), &enrolled));
        assert!(gate_allows(false, Some("beta"), &enrolled));
        assert!(!gate_allows(false, Some("gamma"), &enrolled));
        assert!(!gate_allows(false, None, &enrolled), "absent SNI refused");
        assert!(!gate_allows(false, Some("alpha"), &BTreeSet::new()));
    }

    #[test]
    fn enrolled_routes_fails_closed_on_malformed_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay-tokens");
        fs::write(&path, "not a valid line\n").unwrap();
        assert!(enrolled_routes(&path).is_empty());
    }

    #[test]
    fn enrolled_routes_reflects_the_file_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay-tokens");
        assert!(enrolled_routes(&path).is_empty(), "missing file: no routes");

        // Pair-while-running: a mint is visible on the very next call, and
        // deleting the file revokes on the next call — no reload machinery.
        crate::tokens::mint_route_token(&path, "alpha").unwrap();
        assert!(enrolled_routes(&path).contains("alpha"));
        fs::remove_file(&path).unwrap();
        assert!(enrolled_routes(&path).is_empty());
    }

    #[test]
    fn ensure_self_signed_provisions_then_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("relay-cert.pem");
        let key = dir.path().join("relay-key.pem");

        ensure_self_signed(&cert, &key).unwrap();
        assert!(cert.exists() && key.exists());

        let key_mode = fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(key_mode, 0o600, "private key must be owner-only");

        // Idempotent: a second call does not regenerate, so the pinned
        // fingerprint stays stable across restarts.
        let fp1 = cert_fingerprint(&cert).unwrap();
        ensure_self_signed(&cert, &key).unwrap();
        let fp2 = cert_fingerprint(&cert).unwrap();
        assert_eq!(fp1, fp2);

        // Fingerprint shape: 32 SHA-256 bytes as colon-separated hex pairs.
        assert_eq!(fp1.matches(':').count(), 31);
        assert!(fp1.bytes().all(|b| b.is_ascii_hexdigit() || b == b':'));
    }

    #[test]
    fn server_config_builds_with_both_alpns_and_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("relay-cert.pem");
        let key = dir.path().join("relay-key.pem");
        let tokens = dir.path().join("relay-tokens");
        ensure_self_signed(&cert, &key).unwrap();

        let config = server_config(&cert, &key, &tokens).unwrap();
        assert_eq!(
            config.alpn_protocols,
            vec![QUIC_RELAY_ALPN.to_vec(), QUIC_ALPN.to_vec()],
            "one endpoint, both legs"
        );
    }

    #[test]
    fn server_config_errors_on_missing_material() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.pem");
        let tokens = dir.path().join("relay-tokens");
        assert!(server_config(&missing, &missing, &tokens).is_err());
        assert!(cert_fingerprint(&missing).is_err());
    }
}
