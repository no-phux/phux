//! The client half of workload certificate enrollment (ADR-0116,
//! `workload-auth.md` §8): the private key is generated here and never
//! leaves; only a CSR travels to the authority, and what comes back is
//! validated in full before anything is stored.
//!
//! [`ClientRequest::generate`] mints the key and its CSR.
//! [`ClientRequest::accept`] takes the issuing host's reply, a PEM chain of
//! exactly the issued leaf then the CA, and refuses it unless the leaf carries
//! this request's own key, is a client-authentication leaf that verifies
//! against that CA now (the same verifier the TLS acceptor uses), and is not
//! itself a CA. [`IssuedIdentity::store`] writes the key and chain owner-only
//! under the workload store's lock, temp-file-and-rename, never following a
//! link. No diagnostic here carries key bytes, and `Debug` withholds them.

use std::path::Path;

use rcgen::{CertificateParams, DnType, KeyPair, PublicKeyData};
use rustls::pki_types::pem::{PemObject, SectionKind};
use rustls::pki_types::{CertificateDer, UnixTime};

use super::{MAX_MATERIAL_BYTES, WorkloadError, credential_id, scrub, store};

/// Suffix of every PEM private-key label; a reply carrying one is refused.
const PRIVATE_KEY_MARKER: &str = "PRIVATE KEY";

/// A freshly generated client key and the CSR that asks for it to be
/// certified. The key never leaves this value except through
/// [`IssuedIdentity::store`].
pub struct ClientRequest {
    key: KeyPair,
    public_key: Vec<u8>,
    csr_pem: String,
}

impl std::fmt::Debug for ClientRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientRequest")
            .field("credential_id", &self.credential_id())
            .field("key", &"[withheld]")
            .finish_non_exhaustive()
    }
}

impl ClientRequest {
    /// Generate a key (ECDSA P-256) and a CSR for it. The CSR names nothing
    /// the authority keeps: it certifies only the public key.
    ///
    /// # Errors
    ///
    /// [`WorkloadError::Certificate`] if generation or signing fails.
    pub fn generate() -> Result<Self, WorkloadError> {
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::default();
        params
            .distinguished_name
            .push(DnType::CommonName, "phux workload client");
        let csr_pem = params.serialize_request(&key)?.pem()?;
        Ok(Self {
            public_key: key.subject_public_key_info(),
            key,
            csr_pem,
        })
    }

    /// The CSR, PEM. Public: it is the only thing sent to the authority.
    #[must_use]
    pub fn csr_pem(&self) -> &str {
        &self.csr_pem
    }

    /// The credential id the authority will record for this key.
    #[must_use]
    pub fn credential_id(&self) -> String {
        credential_id(&self.public_key)
    }

    /// Validate the authority's reply and bind it to this key.
    ///
    /// `chain_pem` must be exactly two PEM certificates, the issued leaf then
    /// the CA, and nothing else. The leaf must carry this request's public
    /// key, verify against that CA as a client certificate valid now, and not
    /// be a CA itself.
    ///
    /// # Errors
    ///
    /// [`WorkloadError::IssuedMismatch`] naming the rule the reply broke; the
    /// reply is never echoed.
    pub fn accept(&self, chain_pem: &str) -> Result<IssuedIdentity, WorkloadError> {
        let (leaf, ca) = parse_reply(chain_pem)?;
        let leaf_key = super::subject_public_key_info(leaf.as_ref())
            .map_err(|_| mismatch("the issued certificate is not valid X.509"))?;
        if leaf_key != self.public_key {
            return Err(mismatch(
                "the issued certificate does not carry the key this request generated",
            ));
        }
        refuse_ca_leaf(&leaf)?;
        let verifier = crate::transport::tls::client_verifier(&ca).map_err(|_| {
            mismatch("the returned CA certificate cannot verify client certificates")
        })?;
        verifier
            .verify_client_cert(&leaf, &[], UnixTime::now())
            .map_err(|_| {
                mismatch(
                    "the issued certificate does not verify against the returned CA as a current client certificate",
                )
            })?;
        // Moved, not copied: the one buffer is scrubbed when the identity
        // drops.
        Ok(IssuedIdentity {
            key_pem: self.key.serialize_pem().into_bytes(),
            chain_pem: chain_pem.to_owned(),
            credential_id: credential_id(&self.public_key),
            ca_fingerprint: credential_id(ca.as_ref()),
        })
    }
}

/// A validated client identity: the private key, its certificate chain
/// (leaf then CA), and the names derived from them.
pub struct IssuedIdentity {
    key_pem: Vec<u8>,
    chain_pem: String,
    credential_id: String,
    ca_fingerprint: String,
}

impl std::fmt::Debug for IssuedIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedIdentity")
            .field("credential_id", &self.credential_id)
            .field("ca_fingerprint", &self.ca_fingerprint)
            .field("key", &"[withheld]")
            .finish_non_exhaustive()
    }
}

impl Drop for IssuedIdentity {
    fn drop(&mut self) {
        scrub(&mut self.key_pem);
    }
}

impl IssuedIdentity {
    /// The credential id the authority recorded (`sha256:` of the key).
    #[must_use]
    pub fn credential_id(&self) -> &str {
        &self.credential_id
    }

    /// The `sha256:` fingerprint of the CA that issued the certificate.
    #[must_use]
    pub fn ca_fingerprint(&self) -> &str {
        &self.ca_fingerprint
    }

    /// Write the key to `key_path` and the chain to `cert_path`, both mode
    /// 0600, under the lock of the key's directory. Both files must be new
    /// names in one owner-only directory; a failure removes whatever this
    /// call wrote.
    ///
    /// # Errors
    ///
    /// [`WorkloadError::ProductionState`] for a development build aimed at
    /// production state, [`WorkloadError::Insecure`] for a directory another
    /// user controls, [`WorkloadError::AlreadyStored`] if either file exists,
    /// or the I/O failure.
    pub fn store(&self, key_path: &Path, cert_path: &Path) -> Result<(), WorkloadError> {
        if store::parent_of(key_path) != store::parent_of(cert_path) {
            return Err(WorkloadError::Insecure {
                file: "client identity",
                reason: "must keep its key and certificate in one directory",
            });
        }
        store::with_lock(key_path, |held| {
            for path in [key_path, cert_path] {
                if std::fs::symlink_metadata(path).is_ok() {
                    return Err(WorkloadError::AlreadyStored);
                }
            }
            let written = store::atomic_replace(key_path, &self.key_pem, held)
                .and_then(|()| store::atomic_replace(cert_path, self.chain_pem.as_bytes(), held));
            if written.is_err() {
                remove_identity_files(key_path, cert_path);
            }
            written
        })
    }
}

/// Remove a stored client identity's two files, best effort: only regular
/// files the effective user owns, never through a link. A missing file is
/// already gone.
pub fn remove_identity_files(key_path: &Path, cert_path: &Path) {
    for path in [key_path, cert_path] {
        if store::is_owned_regular_file(path) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The credential id of the stored client certificate at `cert_path`.
///
/// `None` when the file is missing or holds no certificate. Public material:
/// it names the credential a re-enrollment replaces.
#[must_use]
pub fn stored_credential_id(cert_path: &Path) -> Option<String> {
    let bytes = std::fs::read(cert_path).ok()?;
    let leaf = CertificateDer::pem_slice_iter(&bytes).next()?.ok()?;
    super::subject_public_key_info(leaf.as_ref())
        .ok()
        .map(|key| credential_id(&key))
}

const fn mismatch(reason: &'static str) -> WorkloadError {
    WorkloadError::IssuedMismatch(reason)
}

/// Exactly `[leaf, CA]` as PEM certificates: bounded, no private key, no
/// other section.
fn parse_reply(
    chain_pem: &str,
) -> Result<(CertificateDer<'static>, CertificateDer<'static>), WorkloadError> {
    if chain_pem.len() > MAX_MATERIAL_BYTES {
        return Err(mismatch("the issued certificate chain exceeds 64 KiB"));
    }
    if chain_pem.contains(PRIVATE_KEY_MARKER) {
        return Err(mismatch("the reply carries a private key"));
    }
    let sections = <(SectionKind, Vec<u8>)>::pem_slice_iter(chain_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| mismatch("the issued certificate chain is not PEM"))?;
    match sections.as_slice() {
        [
            (SectionKind::Certificate, leaf),
            (SectionKind::Certificate, ca),
        ] => Ok((
            CertificateDer::from(leaf.clone()),
            CertificateDer::from(ca.clone()),
        )),
        _ => Err(mismatch(
            "the reply must be exactly the issued certificate then the CA certificate",
        )),
    }
}

/// An issued client leaf must not be a CA: nothing it signs is trusted.
fn refuse_ca_leaf(leaf: &CertificateDer<'_>) -> Result<(), WorkloadError> {
    let (_, parsed) = x509_parser::parse_x509_certificate(leaf.as_ref())
        .map_err(|_| mismatch("the issued certificate is not valid X.509"))?;
    if parsed.is_ca() {
        return Err(mismatch("the issued certificate is a CA certificate"));
    }
    Ok(())
}
