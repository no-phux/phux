//! The server certificate the workload CA issues, and CA rotation (ADR-0153).
//!
//! A server provisioning its TLS pair for the first time asks the workload CA
//! for it, so clients can pin the CA instead of one leaf
//! (`workload-auth.md` §2). The certificate file holds the chain, leaf then
//! CA, which is how a client learns which authority issued the leaf. An
//! existing pair is never re-issued here: devices pin its leaf.
//!
//! Rotation is the explicit, operator-run exception: a new CA, and a new
//! server certificate under it naming the old one's addresses. Every client
//! pinned the old CA or leaf, and every client certificate chains to the old
//! CA, so every client re-pairs; there is no cross-signing. The replaced
//! files are kept beside the new ones, owner-only, as `*.retired-<unix>`.

use std::path::Path;

use rcgen::{
    CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;

use super::store::{self, FileRole};
use super::{
    WorkloadError, WorkloadPaths, credential_id, ensure_ca, load_authority_certificate,
    load_authority_key, scrub,
};

/// The common name every issued server certificate carries. Clients pin; they
/// never read it.
const SERVER_COMMON_NAME: &str = "phux server";

/// A server certificate chain and its private key, as PEM. The key is
/// withheld from `Debug` and scrubbed (best effort) on drop.
pub struct IssuedServerIdentity {
    chain_pem: String,
    key_pem: String,
}

impl IssuedServerIdentity {
    /// The certificate chain: the issued leaf, then the CA. Public.
    #[must_use]
    pub fn chain_pem(&self) -> &str {
        &self.chain_pem
    }

    /// The leaf's private key. Secret: write it owner-only, never log it.
    #[must_use]
    pub fn key_pem(&self) -> &str {
        &self.key_pem
    }
}

impl std::fmt::Debug for IssuedServerIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedServerIdentity")
            .field("chain_pem", &self.chain_pem)
            .field("key_pem", &"[withheld]")
            .finish()
    }
}

impl Drop for IssuedServerIdentity {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.key_pem).into_bytes();
        scrub(&mut bytes);
    }
}

/// Issue a server certificate naming `sans` from the workload CA at `paths`,
/// creating the CA first if neither half exists (first routable listen,
/// ADR-0116).
///
/// # Errors
///
/// A partial, insecure, or unreadable CA, a refused production path in a
/// development build, or a signing failure.
pub fn issue_server_identity(
    paths: &WorkloadPaths,
    sans: &[String],
) -> Result<IssuedServerIdentity, WorkloadError> {
    ensure_ca(&paths.ca_cert, &paths.ca_key)?;
    let (ca_certificate, ca_pem) = load_authority_certificate(&paths.ca_cert)?;
    issue_under(
        &ca_certificate,
        &ca_pem,
        load_authority_key(&paths.ca_key)?,
        sans,
    )
}

fn issue_under(
    ca_certificate: &CertificateDer<'static>,
    ca_pem: &str,
    ca_key: KeyPair,
    sans: &[String],
) -> Result<IssuedServerIdentity, WorkloadError> {
    let issuer = Issuer::from_ca_cert_der(ca_certificate, ca_key)?;
    let mut params = CertificateParams::new(sans.to_vec())?;
    params
        .distinguished_name
        .push(DnType::CommonName, SERVER_COMMON_NAME);
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    // rcgen's default validity (1975 to 4096) matches the self-signed pair
    // this replaces: a leaf is re-issued deliberately, never by expiry.
    let key = KeyPair::generate()?;
    let certificate = params.signed_by(&key, &issuer)?;
    Ok(IssuedServerIdentity {
        chain_pem: format!("{}{ca_pem}", certificate.pem()),
        key_pem: key.serialize_pem(),
    })
}

/// The server TLS pair a rotation re-issues: the default, phux-provisioned
/// files. An operator-supplied certificate is never touched.
#[derive(Debug, Clone, Copy)]
pub struct ServerPair<'a> {
    /// The certificate (chain) file.
    pub cert: &'a Path,
    /// Its private key.
    pub key: &'a Path,
}

/// What [`rotate_authority`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rotation {
    /// `sha256:` fingerprint of the CA it replaced.
    pub previous: String,
    /// `sha256:` fingerprint of the new CA.
    pub fingerprint: String,
    /// The new server leaf's fingerprint in the `phux pair` shape, when a
    /// server pair was re-issued.
    pub server_leaf: Option<String>,
    /// The suffix the replaced files now carry (`retired-<unix>`).
    pub retired_suffix: String,
}

/// Replace the workload CA with a new one and, when `server` names an
/// existing pair, re-issue the server certificate under it with the same
/// addresses. The replaced files are kept as `<name>.retired-<unix>`.
///
/// Everything is minted in memory first and written under the CA
/// directory's lock: the retired copies, then the server pair, then the CA
/// key and certificate.
///
/// # Errors
///
/// [`WorkloadError::AuthorityMissing`] when there is no CA to rotate (run
/// `phux workload authority --init`), and any read, mint, or write failure.
pub fn rotate_authority(
    paths: &WorkloadPaths,
    server: Option<ServerPair<'_>>,
) -> Result<Rotation, WorkloadError> {
    let stamp = chrono::Utc::now().timestamp();
    let suffix = format!("retired-{stamp}");
    store::with_lock(&paths.ca_key, |held| {
        let (old_certificate, old_pem) = load_authority_certificate(&paths.ca_cert)?;
        let Some((_, mut old_key)) = store::read_stable(&paths.ca_key, FileRole::CaPrivateKey)?
        else {
            return Err(WorkloadError::PartialPair {
                present: "certificate",
                missing: "private key",
            });
        };
        let (new_key, new_certificate) = new_authority()?;
        let new_pem = new_certificate.pem();
        let new_der = CertificateDer::from(new_certificate.der().to_vec());
        let server = server.filter(|pair| pair.cert.exists() && pair.key.exists());
        let reissued = server
            .map(|pair| {
                let issuer_key = KeyPair::from_pem(&new_key.serialize_pem())?;
                issue_under(&new_der, &new_pem, issuer_key, &leaf_names(pair.cert))
            })
            .transpose()?;

        let retire =
            |path: &Path, bytes: &[u8]| store::atomic_replace(&retired(path, &suffix), bytes, held);
        let retired_key = retire(&paths.ca_key, &old_key);
        scrub(&mut old_key);
        retired_key?;
        retire(&paths.ca_cert, old_pem.as_bytes())?;
        let server_leaf = if let (Some(pair), Some(issued)) = (server, &reissued) {
            let mut key = std::fs::read(pair.key)?;
            let retired_server_key = retire(pair.key, &key);
            scrub(&mut key);
            retired_server_key?;
            retire(pair.cert, &std::fs::read(pair.cert)?)?;
            store::atomic_replace(pair.key, issued.key_pem().as_bytes(), held)?;
            store::atomic_replace(pair.cert, issued.chain_pem().as_bytes(), held)?;
            Some(leaf_fingerprint(issued.chain_pem())?)
        } else {
            None
        };
        let mut key_pem = new_key.serialize_pem().into_bytes();
        let written = store::atomic_replace(&paths.ca_key, &key_pem, held);
        scrub(&mut key_pem);
        written?;
        store::atomic_replace(&paths.ca_cert, new_pem.as_bytes(), held)?;
        Ok(Rotation {
            previous: credential_id(old_certificate.as_ref()),
            fingerprint: credential_id(new_der.as_ref()),
            server_leaf,
            retired_suffix: suffix.clone(),
        })
    })
}

/// A new CA key and self-signed certificate, with the fields a first one
/// gets ([`super::authority_params`]).
fn new_authority() -> Result<(KeyPair, rcgen::Certificate), WorkloadError> {
    let params = super::authority_params()?;
    let key = KeyPair::generate()?;
    let certificate = params.self_signed(&key)?;
    Ok((key, certificate))
}

/// `<path>.retired-<unix>` beside `path`.
fn retired(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

/// The addresses the current leaf names, so its replacement names the same
/// ones. A leaf that does not parse yields the loopback names alone.
fn leaf_names(cert: &Path) -> Vec<String> {
    let names = std::fs::read(cert)
        .ok()
        .and_then(|pem| CertificateDer::pem_slice_iter(&pem).next()?.ok())
        .map(|leaf| subject_alt_names(leaf.as_ref()))
        .unwrap_or_default();
    phux_dial::cert::san_list(&names)
}

fn subject_alt_names(der: &[u8]) -> Vec<String> {
    use x509_parser::extensions::GeneralName;
    let Ok((_, parsed)) = x509_parser::parse_x509_certificate(der) else {
        return Vec::new();
    };
    let Ok(Some(sans)) = parsed.subject_alternative_name() else {
        return Vec::new();
    };
    sans.value
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::DNSName(dns) => Some((*dns).to_owned()),
            GeneralName::IPAddress(bytes) => ip_literal(bytes),
            _ => None,
        })
        .collect()
}

fn ip_literal(bytes: &[u8]) -> Option<String> {
    match bytes.len() {
        4 => <[u8; 4]>::try_from(bytes)
            .ok()
            .map(|octets| std::net::Ipv4Addr::from(octets).to_string()),
        16 => <[u8; 16]>::try_from(bytes)
            .ok()
            .map(|octets| std::net::Ipv6Addr::from(octets).to_string()),
        _ => None,
    }
}

/// The `phux pair` fingerprint of a chain's leaf.
fn leaf_fingerprint(chain_pem: &str) -> Result<String, WorkloadError> {
    use sha2::{Digest, Sha256};
    let leaf = CertificateDer::pem_slice_iter(chain_pem.as_bytes())
        .next()
        .and_then(Result::ok)
        .ok_or(WorkloadError::MalformedAuthority(
            "the issued server certificate is not PEM",
        ))?;
    let digest = Sha256::digest(leaf.as_ref());
    Ok(digest
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_dial::tls::chain_authority;
    use rustls::pki_types::UnixTime;

    fn paths(dir: &Path) -> WorkloadPaths {
        WorkloadPaths {
            ca_cert: dir.join("workload-ca.pem"),
            ca_key: dir.join("workload-ca.key"),
            registry: dir.join("workload-keys"),
        }
    }

    fn chain(pem: &str) -> Vec<CertificateDer<'static>> {
        CertificateDer::pem_slice_iter(pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn state_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            dir.path(),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        dir
    }

    /// The issued chain is leaf then CA, the leaf verifies under the CA for
    /// server authentication, and the authority a client reads off it is the
    /// fingerprint `phux workload authority` prints.
    #[test]
    fn an_issued_server_certificate_chains_to_the_authority_it_names() {
        let dir = state_dir();
        let paths = paths(dir.path());
        let issued = issue_server_identity(&paths, &["localhost".to_owned()]).unwrap();
        let certs = chain(issued.chain_pem());
        assert_eq!(certs.len(), 2, "leaf, then the CA");
        let authority = chain_authority(&certs[0], &certs[1..], UnixTime::now());
        assert_eq!(
            authority.as_deref(),
            Some(
                super::super::ca_fingerprint(&paths.ca_cert)
                    .unwrap()
                    .as_str()
            )
        );
        assert!(!format!("{issued:?}").contains("PRIVATE KEY"));
    }

    /// Rotation mints a new CA, re-issues the server pair under it with the
    /// old leaf's names, and keeps every replaced file owner-only.
    #[test]
    fn rotation_replaces_the_authority_and_the_server_certificate() {
        let dir = state_dir();
        let paths = paths(dir.path());
        let cert = dir.path().join("remote-cert.pem");
        let key = dir.path().join("remote-key.pem");
        let issued =
            issue_server_identity(&paths, &["localhost".to_owned(), "100.64.0.2".to_owned()])
                .unwrap();
        phux_dial::cert::write_pair(&cert, &key, issued.chain_pem(), issued.key_pem()).unwrap();
        let before = super::super::ca_fingerprint(&paths.ca_cert).unwrap();

        let rotation = rotate_authority(
            &paths,
            Some(ServerPair {
                cert: &cert,
                key: &key,
            }),
        )
        .unwrap();
        assert_eq!(rotation.previous, before);
        let after = super::super::ca_fingerprint(&paths.ca_cert).unwrap();
        assert_eq!(rotation.fingerprint, after);
        assert_ne!(before, after, "a new authority");

        let certs = chain(&std::fs::read_to_string(&cert).unwrap());
        let presented = chain_authority(&certs[0], &certs[1..], UnixTime::now());
        assert_eq!(presented.as_deref(), Some(after.as_str()));
        assert_eq!(
            rotation.server_leaf.as_deref(),
            Some(phux_dial::cert::cert_fingerprint(&cert).unwrap().as_str())
        );
        assert!(
            leaf_names(&cert).contains(&"100.64.0.2".to_owned()),
            "the re-issued leaf keeps the old addresses"
        );
        for name in [
            "workload-ca.pem",
            "workload-ca.key",
            "remote-cert.pem",
            "remote-key.pem",
        ] {
            let retired = dir
                .path()
                .join(format!("{name}.{}", rotation.retired_suffix));
            let mode = std::fs::metadata(&retired).unwrap().permissions();
            assert_eq!(
                std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
                0o600,
                "{name} is retired owner-only"
            );
        }
    }

    /// There is nothing to rotate before an authority exists.
    #[test]
    fn rotation_needs_an_existing_authority() {
        let dir = state_dir();
        assert!(matches!(
            rotate_authority(&paths(dir.path()), None),
            Err(WorkloadError::AuthorityMissing)
        ));
    }
}
