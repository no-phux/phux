//! TLS termination for the remote-consumer WebSocket listener (ADR-0031).
//!
//! Encryption for `wss://` remote consumers is TLS 1.3 (with 1.2 as a floor)
//! via rustls, terminated here before the RFC 6455 upgrade. The operator
//! supplies a PEM certificate chain and private key; this module turns them
//! into a [`TlsAcceptor`] the listener wraps each accepted TCP stream in.
//!
//! The `ring` crypto provider is selected explicitly (`builder_with_provider`)
//! rather than relying on a process-default install, both because only `ring`
//! is compiled in and so the choice is visible at the call site.
//!
//! [`cert_fingerprint`] computes the SHA-256 of the leaf certificate for the
//! out-of-band pin shown at pairing time (`phux pair`), closing the
//! trust-on-first-use gap ADR-0031 names.
//!
//! [`ensure_self_signed_for`] names the *advertised* address in the SANs, and
//! [`covers_name`] reports whether an already-persisted certificate does —
//! never repairing it, because widening SANs means a new certificate and a new
//! fingerprint (ADR-0091).
//!
//! Generation and PEM loading themselves are [`phux_dial::cert`]'s: `phux-relay`
//! terminates TLS on identical terms and used to carry a near-verbatim copy of
//! them, and ADR-0051 forbids it depending on this crate. What remains here is
//! what is genuinely the server's — the acceptor and the three listener
//! configs, plus the ADR-0091 *reporting* surface (`covers_name`,
//! `uncovered_names`, `advertised_for_bind`), which has no counterpart in the
//! relay and stays where its callers are.

use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use phux_dial::cert;
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use tokio_rustls::TlsAcceptor;

/// Errors from loading TLS material or building the acceptor.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// A certificate or key file could not be read.
    #[error("tls io: {0}")]
    Io(#[from] io::Error),
    /// The certificate file held no certificates.
    #[error("no certificates in {0}")]
    NoCerts(String),
    /// A PEM certificate or key file could not be parsed.
    #[error("pem: {0}")]
    Pem(#[from] rustls::pki_types::pem::Error),
    /// rustls rejected the certificate/key pair.
    #[error("rustls: {0}")]
    Rustls(#[from] rustls::Error),
    /// Generating the self-signed certificate failed.
    #[error("certificate generation: {0}")]
    Rcgen(#[from] rcgen::Error),
    /// Exactly one of the persisted cert/key pair exists. Regenerating
    /// would silently rotate the fingerprint pinned on every paired
    /// device, so the operator must delete the survivor explicitly.
    #[error(
        "partial TLS pair: {present} exists but {missing} is missing — delete {present} to regenerate (this rotates the pinned fingerprint and breaks existing pins)"
    )]
    PartialTlsPair {
        /// Path of the file that still exists.
        present: String,
        /// Path of the file that is missing.
        missing: String,
    },
}

/// Provisioning lives in [`phux_dial::cert`] so `phux-relay` can share it
/// (ADR-0051), but this crate's error vocabulary is still this crate's.
///
/// Every arm maps onto a variant [`TlsError`] already had, carrying the same
/// payload, so the message an operator reads is byte-for-byte what it was
/// before the move — including `tls io:`, which `phux-relay` renders as
/// `relay io:` off the identical shared error. Mapping rather than
/// re-exporting is what keeps those two free to differ.
impl From<cert::CertError> for TlsError {
    fn from(err: cert::CertError) -> Self {
        match err {
            cert::CertError::Io(err) => Self::Io(err),
            cert::CertError::Rcgen(err) => Self::Rcgen(err),
            cert::CertError::Pem(err) => Self::Pem(err),
            cert::CertError::NoCerts(path) => Self::NoCerts(path),
            cert::CertError::PartialTlsPair { present, missing } => {
                Self::PartialTlsPair { present, missing }
            }
        }
    }
}

/// Default persisted path for the auto-generated remote-consumer certificate:
/// `<state-dir>/remote-cert.pem`.
#[must_use]
pub fn default_cert_path() -> PathBuf {
    crate::telemetry::state_dir().join("remote-cert.pem")
}

/// Default persisted path for the auto-generated remote-consumer private key:
/// `<state-dir>/remote-key.pem`.
#[must_use]
pub fn default_key_path() -> PathBuf {
    crate::telemetry::state_dir().join("remote-key.pem")
}

/// The SAN vocabulary this module reports against, owned by
/// [`phux_dial::cert`] alongside the generator that applies it. Imported for
/// the tests that assert the shape directly; production code reaches it
/// through [`ensure_self_signed_for`].
#[cfg(test)]
use cert::{LOOPBACK_SANS, san_list};

/// Provision a self-signed certificate + key at the given paths if either is
/// missing, naming only the loopback identities.
///
/// The address-agnostic form, for call sites with no advertised address to
/// name (tests, and any provisioning that happens before an address is known).
/// Prefer [`ensure_self_signed_for`] wherever the routable address *is* known:
/// SANs can only be chosen when the certificate is minted.
pub fn ensure_self_signed(cert_path: &Path, key_path: &Path) -> Result<(), TlsError> {
    Ok(cert::ensure_self_signed(cert_path, key_path)?)
}

/// Provision a self-signed certificate + key at the given paths if either is
/// missing, naming `advertised` in the SANs alongside the loopback identities.
///
/// This is what lets a remote listener need no operator cert setup (ADR-0031
/// "seamless"). A complete pair is left untouched, so the fingerprint stays
/// stable across restarts once pinned on a device.
///
/// The certificate is public (world-readable); the private key is written
/// owner-only (`0o600`). SANs always cover `localhost`/`127.0.0.1`/`::1`; `advertised`
/// adds the addresses phux hands out — the listener's own routable bind and
/// the overlay address `phux pair` embeds in its connect link. Each entry may
/// be an IP literal or a DNS name; rcgen classifies it by whether it parses as
/// an address. Duplicates and empties are dropped, and order is preserved so
/// the SAN list is a stable function of its input.
///
/// **A certificate that already exists is never widened** (ADR-0091). Adding a
/// SAN means minting a new certificate, which means a new fingerprint, which
/// un-pairs every device that pinned the old one — a silent, total trust break
/// traded for a handshake convenience. Coverage is therefore *reported* rather
/// than repaired: see [`covers_name`], `phux doctor`'s `remote-cert` check, and
/// the warning `phux pair` prints next to the link it is about to hand out.
pub fn ensure_self_signed_for(
    cert_path: &Path,
    key_path: &Path,
    advertised: &[String],
) -> Result<(), TlsError> {
    Ok(cert::ensure_self_signed_for(
        cert_path, key_path, advertised,
    )?)
}

/// The SANs a listener bound to `addr` advertises: its own address, when that
/// is an address a remote consumer could actually dial.
///
/// A loopback bind adds nothing (the always-present loopback SANs name it), and an
/// unspecified bind (`0.0.0.0` / `[::]`) names no address at all — the kernel
/// picks one per interface, so there is nothing here to put in a SAN. The
/// routable address in that case is known only to `phux pair`, which detects
/// the overlay (ADR-0037) and passes it in directly.
///
/// The auto-bound overlay listener (ADR-0081) always binds a *specific*
/// detected address, so it is the common case that this covers — and it costs
/// no detection call of its own, because the address is already in hand by the
/// time the listener is built (phux-90j5 keeps detection off the startup path
/// and this must not put it back).
#[must_use]
pub fn advertised_for_bind(addr: std::net::SocketAddr) -> Vec<String> {
    let ip = addr.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        Vec::new()
    } else {
        vec![san_name(ip)]
    }
}

/// The SAN form of an advertised address: a bare IP literal, unbracketed.
///
/// A v6 address reaches this from a URL as `[fd7a::1]` but must go into a SAN
/// (and into [`covers_name`]) as `fd7a::1` — `ServerName` and rcgen both take
/// the address, never the URL-authority bracketing.
#[must_use]
pub fn san_name(addr: IpAddr) -> String {
    addr.to_string()
}

/// Whether a conventionally-validating client would accept `name` for the
/// certificate at `cert_path`.
///
/// This is not a SAN-list string comparison: it runs the certificate through
/// rustls' own [`rustls::client::verify_server_name`] — the same webpki name
/// check a rustls client performs mid-handshake — so a `true` here is the
/// property that actually matters (the client accepts the name), not a proxy
/// for it. IP literals are matched as `iPAddress` SANs and everything else as
/// `dNSName`, exactly as on the wire.
///
/// Name validation is only *one* of the checks such a client makes; it must
/// also trust the certificate, which for a self-signed leaf means the operator
/// added it to a trust store or pinned it. phux's own consumers pin the
/// SHA-256 fingerprint and ignore the name entirely, so this reports the
/// third-party path, not the phux one.
///
/// `Err` means the certificate could not be read or parsed at all; a name that
/// simply is not covered is `Ok(false)`.
pub fn covers_name(cert_path: &Path, name: &str) -> Result<bool, TlsError> {
    let certs = load_certs(cert_path)?;
    let leaf = certs
        .first()
        .ok_or_else(|| TlsError::NoCerts(cert_path.display().to_string()))?;
    let parsed = rustls::server::ParsedCertificate::try_from(leaf)?;
    // An unparseable server name cannot be covered by any certificate, and is
    // not a defect in the certificate — report it as uncovered.
    let Ok(server_name) = ServerName::try_from(name.to_owned()) else {
        return Ok(false);
    };
    Ok(rustls::client::verify_server_name(&parsed, &server_name).is_ok())
}

/// Which of `names` the certificate at `cert_path` does **not** cover, in the
/// order given. Empty when every name verifies (the healthy case).
///
/// The reporting counterpart to [`ensure_self_signed_for`]: provisioning
/// chooses SANs once, and every later caller can only ask what it got.
pub fn uncovered_names(cert_path: &Path, names: &[String]) -> Result<Vec<String>, TlsError> {
    let mut missing = Vec::new();
    for name in names {
        if !covers_name(cert_path, name)? {
            missing.push(name.clone());
        }
    }
    Ok(missing)
}

/// ALPN protocol id advertised on the QUIC listener. Owned by `phux-protocol`
/// (the wire crate) so the server listener and the client dialer cannot drift;
/// re-exported here for the QUIC transport's call sites and tests.
pub(crate) use phux_protocol::policy::QUIC_ALPN;

/// Build a rustls [`ServerConfig`] from a PEM certificate chain and private
/// key, using the `ring` provider. Shared by the WebSocket [`TlsAcceptor`] and
/// the QUIC listener so both terminate TLS with the identical cert material.
///
/// `cert_path` is a PEM file with the leaf certificate first, followed by any
/// intermediates; `key_path` is a PEM file with one PKCS#8 / SEC1 / PKCS#1
/// private key. No client authentication is required — the bearer token
/// (see [`crate::auth`]) is the authentication layer; TLS provides encryption
/// and server identity only. Mutual TLS is the ADR-0031 v0.2 hardening.
fn server_config_from_pem(cert_path: &Path, key_path: &Path) -> Result<ServerConfig, TlsError> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;

    Ok(
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(TlsError::Rustls)?
            .with_no_client_auth()
            .with_single_cert(certs, key)?,
    )
}

/// Build a [`TlsAcceptor`] for the WebSocket listener from a PEM cert + key.
pub fn acceptor_from_pem(cert_path: &Path, key_path: &Path) -> Result<TlsAcceptor, TlsError> {
    Ok(TlsAcceptor::from(Arc::new(server_config_from_pem(
        cert_path, key_path,
    )?)))
}

/// Build the rustls [`ServerConfig`] for the QUIC listener from a PEM cert +
/// key: TLS 1.3 only (QUIC forbids earlier versions) with the phux ALPN set.
///
/// Returned as a bare rustls config; the QUIC transport wraps it in quinn's
/// `QuicServerConfig`. Reuses the same cert material as the WebSocket path.
pub(crate) fn quic_server_config(
    cert_path: &Path,
    key_path: &Path,
) -> Result<ServerConfig, TlsError> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;

    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(TlsError::Rustls)?
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![QUIC_ALPN.to_vec()];
    Ok(config)
}

/// Build the rustls [`ServerConfig`] for the WebTransport listener from a PEM
/// cert + key: TLS 1.3 only (QUIC forbids earlier versions) with the standard
/// HTTP/3 ALPN.
///
/// Unlike the raw-QUIC listener (which advertises the phux-private
/// [`QUIC_ALPN`]), WebTransport rides HTTP/3, and browsers offer exactly
/// `h3` — a private ALPN would fail every browser handshake. Reuses the same
/// cert material as the `wss://` and QUIC paths so one pinned fingerprint
/// covers all three remote transports.
#[cfg(feature = "webtransport")]
pub(crate) fn webtransport_server_config(
    cert_path: &Path,
    key_path: &Path,
) -> Result<ServerConfig, TlsError> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;

    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(TlsError::Rustls)?
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
    // The IANA-registered HTTP/3 ALPN id. wtransport exports the same bytes
    // (`wtransport::tls::WEBTRANSPORT_ALPN`); spelled literally here so this
    // module stays wtransport-free apart from the cfg gate.
    config.alpn_protocols = vec![b"h3".to_vec()];
    Ok(config)
}

/// SHA-256 fingerprint of the leaf certificate, formatted as uppercase
/// colon-separated hex (`AB:CD:…`) — the conventional shape for an
/// out-of-band pin shown alongside a pairing token.
pub fn cert_fingerprint(cert_path: &Path) -> Result<String, TlsError> {
    Ok(cert::cert_fingerprint(cert_path)?)
}

/// Read the PEM certificate chain.
fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    Ok(cert::load_certs(path)?)
}

/// Read the first PEM private key (PKCS#8, SEC1, or PKCS#1).
fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>, TlsError> {
    Ok(cert::load_key(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn ensure_self_signed_refuses_a_partial_pair() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("remote-cert.pem");
        let key = dir.path().join("remote-key.pem");
        ensure_self_signed(&cert, &key).unwrap();
        let fp = cert_fingerprint(&cert).unwrap();

        // Key lost, cert survives: must refuse, not silently rotate the
        // fingerprint every paired device pins.
        fs::remove_file(&key).unwrap();
        let err = ensure_self_signed(&cert, &key).unwrap_err();
        assert!(matches!(err, TlsError::PartialTlsPair { .. }), "{err}");
        assert_eq!(
            cert_fingerprint(&cert).unwrap(),
            fp,
            "cert must be untouched"
        );

        // Cert lost, key survives: same refusal.
        fs::remove_file(&cert).unwrap();
        fs::write(&key, "not-a-real-key").unwrap();
        let err = ensure_self_signed(&cert, &key).unwrap_err();
        assert!(matches!(err, TlsError::PartialTlsPair { .. }), "{err}");
    }

    #[test]
    fn ensure_self_signed_provisions_then_is_idempotent_and_builds() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("remote-cert.pem");
        let key = dir.path().join("remote-key.pem");

        ensure_self_signed(&cert, &key).unwrap();
        assert!(cert.exists() && key.exists());

        // Private key is owner-only; the certificate is public.
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

        // The generated material builds a working acceptor.
        acceptor_from_pem(&cert, &key).unwrap();
    }

    #[test]
    fn san_list_keeps_loopback_first_and_dedupes_advertised() {
        assert_eq!(san_list(&[]), LOOPBACK_SANS.map(str::to_owned).to_vec());

        // Advertised names follow the loopback set, in the order given.
        assert_eq!(
            san_list(&["100.64.0.2".to_owned(), "mini.tail.ts.net".to_owned()]),
            vec![
                "localhost",
                "127.0.0.1",
                "::1",
                "100.64.0.2",
                "mini.tail.ts.net"
            ]
        );

        // A repeat of a loopback name, a repeat of an advertised name, and an
        // all-whitespace entry all collapse away. rcgen rejects an empty DNS
        // name outright, so letting one through would fail provisioning.
        assert_eq!(
            san_list(&[
                "127.0.0.1".to_owned(),
                "  ".to_owned(),
                "100.64.0.2".to_owned(),
                "100.64.0.2".to_owned(),
            ]),
            vec!["localhost", "127.0.0.1", "::1", "100.64.0.2"]
        );
    }

    #[test]
    fn advertised_for_bind_names_only_a_dialable_address() {
        use std::net::SocketAddr;

        let advertised = |s: &str| advertised_for_bind(s.parse::<SocketAddr>().unwrap());

        // A specific routable bind is the address a consumer dials.
        assert_eq!(advertised("100.64.0.2:8787"), vec!["100.64.0.2"]);
        // v6 goes in unbracketed: a SAN holds the address, not a URL authority.
        assert_eq!(
            advertised("[fd7a:115c:a1e0::1]:8787"),
            vec!["fd7a:115c:a1e0::1"]
        );
        // Loopback adds nothing — LOOPBACK_SANS already names it.
        assert!(advertised("127.0.0.1:8787").is_empty());
        assert!(advertised("[::1]:8787").is_empty());
        // A wildcard bind names no address at all, so there is nothing to put
        // in a SAN. `phux pair` covers that case by detecting the overlay.
        assert!(advertised("0.0.0.0:8787").is_empty());
        assert!(advertised("[::]:8787").is_empty());
    }

    /// An already-provisioned certificate is never widened (ADR-0091):
    /// regenerating rotates the fingerprint every paired device pins, so the
    /// second call must be a no-op even when it asks for a new address.
    #[test]
    fn an_existing_cert_is_never_widened() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("remote-cert.pem");
        let key = dir.path().join("remote-key.pem");

        ensure_self_signed(&cert, &key).unwrap();
        let fp = cert_fingerprint(&cert).unwrap();
        assert!(!covers_name(&cert, "100.64.0.2").unwrap());

        ensure_self_signed_for(&cert, &key, &["100.64.0.2".to_owned()]).unwrap();
        assert_eq!(
            cert_fingerprint(&cert).unwrap(),
            fp,
            "the pinned fingerprint must survive a wider request"
        );
        assert!(
            !covers_name(&cert, "100.64.0.2").unwrap(),
            "the certificate must not have been silently reissued"
        );
        assert_eq!(
            uncovered_names(&cert, &["100.64.0.2".to_owned(), "127.0.0.1".to_owned()]).unwrap(),
            vec!["100.64.0.2"],
            "the gap is reported instead, which is what doctor and pair print"
        );
    }

    #[test]
    fn covers_name_rejects_an_unparseable_name_without_erroring() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("remote-cert.pem");
        let key = dir.path().join("remote-key.pem");
        ensure_self_signed(&cert, &key).unwrap();

        // Not a certificate defect — no certificate can cover it — so this is
        // `Ok(false)` rather than an error that would mask a real one.
        assert!(!covers_name(&cert, "not a valid name").unwrap());
        assert!(!covers_name(&cert, "").unwrap());
    }

    #[test]
    fn acceptor_and_fingerprint_error_on_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing_cert = dir.path().join("nope.pem");
        let missing_key = dir.path().join("nope.key");
        assert!(acceptor_from_pem(&missing_cert, &missing_key).is_err());
        assert!(cert_fingerprint(&missing_cert).is_err());
    }
}
