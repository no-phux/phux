//! `phux workload sync-managed`: replace exactly one organization's records
//! (`docs/remote.md#managed-by-an-organization`).

use std::path::Path;

use chrono::{DateTime, Utc};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, UnixTime};
use serde::Deserialize;

use super::WorkloadRegistry;
use crate::workload::org_roots::replace_org_roots;
use crate::workload::{
    ClientMaterial, EnrolledFor, MaterialError, WorkloadCredential, WorkloadError, credential_id,
    store, validate_expiry, validate_org_name, validate_scopes, validate_subject,
};

/// The document version this reader accepts.
const DOCUMENT_VERSION: u32 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    v: u32,
    org: String,
    /// PEM CA certificates every credential's leaf chains to.
    roots: Vec<String>,
    credentials: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    /// PEM leaf, optionally followed by intermediates.
    certificate: String,
    scopes: Vec<String>,
    principal: EnrolledFor,
    #[serde(default)]
    subject: Option<String>,
    /// Clamped to the leaf's notAfter; the leaf's notAfter when absent.
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

/// What [`WorkloadRegistry::sync_managed`] changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ManagedSync {
    /// Records the organization added.
    pub added: usize,
    /// Records kept by id whose scopes, principal, subject, or expiry changed.
    pub updated: usize,
    /// Records the organization no longer lists (revoked live).
    pub removed: usize,
    /// Records already exactly as listed.
    pub unchanged: usize,
    /// Registry generation after the call (unchanged when nothing changed).
    pub generation: u64,
}

impl WorkloadRegistry {
    /// Replace every record organization `org` manages with the credentials
    /// `document` lists, and its roots with the document's. Records another
    /// organization manages, or none does, are untouched; a record removed
    /// here ends its live connections like a revocation. One invalid entry
    /// refuses the whole document and writes nothing.
    ///
    /// # Errors
    ///
    /// A malformed document, an entry that would not pass `add-key`'s
    /// validation, a leaf that does not chain to the document's roots, a key
    /// enrolled outside `org`, or any failure to read or write the files.
    pub fn sync_managed(
        path: &Path,
        org: &str,
        document: &[u8],
    ) -> Result<ManagedSync, WorkloadError> {
        validate_org_name(org)?;
        let document: Document = serde_json::from_slice(document)
            .map_err(|error| WorkloadError::ManagedDocument(error.to_string()))?;
        if document.v != DOCUMENT_VERSION {
            return Err(WorkloadError::ManagedDocument(format!(
                "unsupported version {}",
                document.v
            )));
        }
        if document.org != org {
            return Err(WorkloadError::ManagedDocument(
                "it names another organization".to_owned(),
            ));
        }
        let roots = parse_roots(&document.roots)?;
        let mut wanted: Vec<WorkloadCredential> = Vec::with_capacity(document.credentials.len());
        for (index, entry) in document.credentials.iter().enumerate() {
            let credential = managed_credential(entry, index + 1, org, &roots)?;
            if wanted.iter().any(|seen| seen.id == credential.id) {
                return Err(WorkloadError::ManagedDocument(
                    "a credential appears twice".to_owned(),
                ));
            }
            wanted.push(credential);
        }
        let pem = document.roots.iter().fold(String::new(), |mut pem, root| {
            pem.push_str(root.trim());
            pem.push('\n');
            pem
        });
        store::with_lock(path, |held| {
            let mut registry = Self::load(path)?;
            if let Some(taken) = wanted.iter().find(|credential| {
                registry.credentials.iter().any(|record| {
                    record.id == credential.id && record.managed.as_deref() != Some(org)
                })
            }) {
                return Err(WorkloadError::AlreadyRegistered(taken.id.clone()));
            }
            // Roots first: until the registry commit below they admit no one.
            replace_org_roots(path, org, &pem, held)?;
            let (previous, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut registry.credentials)
                .into_iter()
                .partition(|record| record.managed.as_deref() == Some(org));
            registry.credentials = kept;
            let mut outcome = ManagedSync::default();
            for mut credential in wanted {
                match previous.iter().find(|old| old.id == credential.id) {
                    Some(old) => {
                        // A local `phux workload revoke` outlasts the next sync.
                        credential.revoked_at = old.revoked_at;
                        if *old == credential {
                            outcome.unchanged += 1;
                        } else {
                            outcome.updated += 1;
                        }
                    }
                    None => outcome.added += 1,
                }
                registry.credentials.push(credential);
            }
            outcome.removed = previous.len() - outcome.unchanged - outcome.updated;
            if outcome.added + outcome.updated + outcome.removed > 0 {
                registry.commit(path, held)?;
            }
            outcome.generation = registry.generation;
            Ok(outcome)
        })
    }
}

/// Every root must be one PEM certificate a verifier can anchor on.
fn parse_roots(roots: &[String]) -> Result<Vec<CertificateDer<'static>>, WorkloadError> {
    roots
        .iter()
        .map(|pem| {
            let mut certificates = CertificateDer::pem_slice_iter(pem.as_bytes());
            match (certificates.next(), certificates.next()) {
                (Some(Ok(root)), None)
                    if rustls::RootCertStore::empty().add(root.clone()).is_ok() =>
                {
                    Ok(root)
                }
                _ => Err(WorkloadError::ManagedDocument(
                    "each root is one PEM CA certificate".to_owned(),
                )),
            }
        })
        .collect()
}

/// One entry as the record it becomes. `index` is 1-based.
fn managed_credential(
    entry: &Entry,
    index: usize,
    org: &str,
    roots: &[CertificateDer<'static>],
) -> Result<WorkloadCredential, WorkloadError> {
    let ClientMaterial::Certificate {
        leaf,
        intermediates,
    } = ClientMaterial::from_pem(entry.certificate.as_bytes())?
    else {
        return Err(MaterialError::Unsupported.into());
    };
    let chains = !roots.is_empty()
        && crate::transport::tls::roots_verifier(roots, false).is_ok_and(|verifier| {
            verifier
                .verify_client_cert(&leaf, &intermediates, UnixTime::now())
                .is_ok()
        });
    if !chains {
        return Err(WorkloadError::NotIssuedByOrg { index });
    }
    validate_scopes(&entry.scopes)?;
    if let Some(subject) = &entry.subject {
        validate_subject(subject)?;
    }
    let (_, parsed) = x509_parser::parse_x509_certificate(leaf.as_ref())
        .map_err(|_| MaterialError::InvalidCertificate)?;
    let not_after = parsed.validity().not_after.timestamp();
    let expires_at = entry
        .expires_at
        .map_or(not_after, |at| at.timestamp().min(not_after));
    validate_expiry(expires_at)?;
    let public_key = parsed.tbs_certificate.subject_pki.raw.to_vec();
    Ok(WorkloadCredential {
        id: credential_id(&public_key),
        public_key,
        scopes: entry.scopes.clone(),
        expires_at: Some(expires_at),
        revoked_at: None,
        recorded_for: Some(entry.principal),
        satellite: None,
        subject: entry.subject.clone(),
        managed: Some(org.to_owned()),
    })
}
