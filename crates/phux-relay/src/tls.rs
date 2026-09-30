//! TLS termination for the relay's single QUIC endpoint.
//!
//! One endpoint advertises both ALPNs (`phux-relay/1` for connectors,
//! `phux-quic/1` for consumers); the leg is read from the negotiated ALPN,
//! never the byte stream (ADR-0051 invariant 7). Consumer hellos whose SNI is
//! absent or not an enrolled route are refused **at the TLS layer** by
//! [`SniGate`] (ADR-0052 Decision 1). Certificate provisioning is
//! [`phux_dial::cert`]'s.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use phux_dial::cert;
use phux_protocol::policy::{QUIC_ALPN, QUIC_RELAY_ALPN};

use crate::RelayError;
use crate::tokens::CachedRouteTokens;

/// Provision the relay's self-signed certificate + key if missing.
///
/// A complete pair is left untouched so pins stay stable. Only loopback SANs:
/// consumers address the relay by route name in SNI and pin the fingerprint.
pub fn ensure_self_signed(cert_path: &Path, key_path: &Path) -> Result<(), RelayError> {
    Ok(cert::ensure_self_signed(cert_path, key_path)?)
}

/// SHA-256 fingerprint of the leaf certificate as uppercase colon-separated
/// hex, the shape `phux pair` prints.
pub fn cert_fingerprint(cert_path: &Path) -> Result<String, RelayError> {
    Ok(cert::cert_fingerprint(cert_path)?)
}

/// The relay's rustls `ServerConfig`: TLS 1.3 only, no client auth, both
/// ALPNs, and the [`SniGate`] resolver.
pub(crate) fn server_config(
    cert_path: &Path,
    key_path: &Path,
    tokens: Arc<CachedRouteTokens>,
) -> Result<rustls::ServerConfig, RelayError> {
    let certs = cert::load_certs(cert_path)?;
    let key = cert::load_key(key_path)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let signing_key = provider.key_provider.load_private_key(key)?;
    let certified = Arc::new(rustls::sign::CertifiedKey::new(certs, signing_key));

    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(RelayError::Rustls)?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(SniGate {
            key: certified,
            tokens,
        }));
    config.alpn_protocols = vec![QUIC_RELAY_ALPN.to_vec(), QUIC_ALPN.to_vec()];
    Ok(config)
}

/// TLS-layer SNI refusal: returning `None` aborts the handshake before any
/// application byte flows.
///
/// The enrolled-route set follows the store file per handshake (re-read
/// only when it changed) so `phux relay pair` is live without a restart; an
/// unreadable store fails closed.
struct SniGate {
    /// The relay's one certified key, served to every admitted hello.
    key: Arc<rustls::sign::CertifiedKey>,
    /// The route-token store; source of the enrolled-route set.
    tokens: Arc<CachedRouteTokens>,
}

/// Redacted: the certified key never appears in logs.
impl std::fmt::Debug for SniGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SniGate")
            .field("tokens", &self.tokens)
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
            enrolled_routes(&self.tokens)
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

/// The gate's decision. A connector hello (relay ALPN) always passes; its
/// authentication is the stream-0 token preamble. A consumer must name an
/// enrolled route via SNI.
fn gate_allows(offers_relay_alpn: bool, sni: Option<&str>, routes: &BTreeSet<String>) -> bool {
    offers_relay_alpn || sni.is_some_and(|name| routes.contains(name))
}

/// The enrolled-route set as the token store stands; empty (fail closed)
/// when the store is unreadable, malformed, or insecure.
fn enrolled_routes(tokens: &CachedRouteTokens) -> BTreeSet<String> {
    tokens.current().routes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// The relay maps a surviving half-pair onto its own error (with the
    /// operator hint) and leaves the certificate untouched.
    #[test]
    fn ensure_self_signed_refuses_a_partial_pair() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("relay-cert.pem");
        let key = dir.path().join("relay-key.pem");
        ensure_self_signed(&cert, &key).unwrap();
        let fp = cert_fingerprint(&cert).unwrap();

        fs::remove_file(&key).unwrap();
        let err = ensure_self_signed(&cert, &key).unwrap_err();
        assert!(matches!(err, RelayError::PartialTlsPair { .. }), "{err}");
        assert_eq!(cert_fingerprint(&cert).unwrap(), fp);
    }

    #[test]
    fn gate_admits_connectors_always_and_consumers_only_for_enrolled_sni() {
        let enrolled: BTreeSet<String> = ["alpha", "beta"].map(str::to_owned).into();
        let empty = BTreeSet::new();
        for (relay_alpn, sni, routes, allowed) in [
            (true, None, &enrolled, true),
            (true, Some("not-enrolled"), &enrolled, true),
            (true, Some("alpha"), &empty, true),
            (false, Some("alpha"), &enrolled, true),
            (false, Some("beta"), &enrolled, true),
            (false, Some("gamma"), &enrolled, false),
            (false, None, &enrolled, false),
            (false, Some("alpha"), &empty, false),
        ] {
            assert_eq!(
                gate_allows(relay_alpn, sni, routes),
                allowed,
                "relay_alpn={relay_alpn} sni={sni:?}"
            );
        }
    }

    #[test]
    fn enrolled_routes_follow_the_file_and_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay-tokens");
        let tokens = CachedRouteTokens::new(path.clone());
        assert!(
            enrolled_routes(&tokens).is_empty(),
            "missing file: no routes"
        );

        crate::tokens::mint_route_token(&path, "alpha").unwrap();
        assert!(enrolled_routes(&tokens).contains("alpha"));
        fs::remove_file(&path).unwrap();
        assert!(enrolled_routes(&tokens).is_empty());

        fs::write(&path, "not a valid line\n").unwrap();
        assert!(
            enrolled_routes(&tokens).is_empty(),
            "malformed: fail closed"
        );
    }

    #[test]
    fn server_config_offers_both_alpns_and_needs_material() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("relay-cert.pem");
        let key = dir.path().join("relay-key.pem");
        let tokens = Arc::new(CachedRouteTokens::new(dir.path().join("relay-tokens")));
        assert!(server_config(&cert, &key, Arc::clone(&tokens)).is_err());

        ensure_self_signed(&cert, &key).unwrap();
        let config = server_config(&cert, &key, tokens).unwrap();
        assert_eq!(
            config.alpn_protocols,
            vec![QUIC_RELAY_ALPN.to_vec(), QUIC_ALPN.to_vec()]
        );
    }
}
