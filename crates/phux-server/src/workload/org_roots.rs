//! Organization roots beside the registry
//! (`docs/remote.md#managed-by-an-organization`): one owner-only
//! `workload-org-roots/<org>.pem` per organization, written by
//! `phux workload sync-managed` and trusted by a listener's client
//! verifier beside this host's CA. A listener re-reads them when one
//! changes, so a sync needs no restart. Trusting a root admits nobody by
//! itself: a certificate still needs an active registry record.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;

use super::WorkloadError;
use super::store::{self, FileRole, Stamp};

/// The directory beside the registry holding each organization's roots.
const ORG_ROOTS_DIR: &str = "workload-org-roots";

/// Where the organization roots live: beside the registry at `registry`, so
/// they move with `PHUX_WORKLOAD_KEYS`.
#[must_use]
pub fn org_roots_dir(registry: &Path) -> PathBuf {
    registry.with_file_name(ORG_ROOTS_DIR)
}

/// Replace organization `org`'s roots with `pem`, or remove them when it is
/// empty. Nothing is written when they are already exactly that. `held` is
/// the caller's registry lock.
pub(super) fn replace_org_roots(
    registry: &Path,
    org: &str,
    pem: &str,
    held: &store::HeldLock<'_>,
) -> Result<(), WorkloadError> {
    let path = org_roots_dir(registry).join(format!("{org}.pem"));
    store::open_owner_dir(&path)?;
    let current = store::read_stable(&path, FileRole::OrgRoots)?.map(|(_, raw)| raw);
    match current {
        Some(_) if pem.is_empty() => fs::remove_file(&path)?,
        None if pem.is_empty() => {}
        Some(raw) if raw == pem.as_bytes() => {}
        _ => store::atomic_replace(&path, pem.as_bytes(), held)?,
    }
    Ok(())
}

/// What a workload client certificate may chain to: this host's CA and,
/// for a listener, every organization's roots, as they are now.
#[derive(Debug, Clone)]
pub struct ClientRoots {
    host: Arc<[CertificateDer<'static>]>,
    org: Option<Arc<OrgRoots>>,
}

impl ClientRoots {
    /// The host CA `ca` plus the organization roots beside `registry`.
    #[must_use]
    pub fn new(ca: CertificateDer<'static>, registry: &Path) -> Self {
        Self {
            host: Arc::from([ca]),
            org: Some(Arc::new(OrgRoots {
                dir: org_roots_dir(registry),
                cached: Mutex::new(None),
            })),
        }
    }

    /// The host CA first, then each organization root. The same `Arc` comes
    /// back until a roots file changes, so a caller can rebuild what it
    /// derives only then. An unreadable or insecure file contributes nothing.
    #[must_use]
    pub fn current(&self) -> Arc<[CertificateDer<'static>]> {
        self.org
            .as_ref()
            .map_or_else(|| Arc::clone(&self.host), |org| org.current(&self.host))
    }
}

/// The host CA alone, with no organization roots.
impl From<CertificateDer<'static>> for ClientRoots {
    fn from(ca: CertificateDer<'static>) -> Self {
        Self {
            host: Arc::from([ca]),
            org: None,
        }
    }
}

/// Each `.pem` file's name and stamp (`None` when it failed its check).
type Observed = Vec<(OsString, Option<Stamp>)>;

#[derive(Debug)]
struct OrgRoots {
    dir: PathBuf,
    cached: Mutex<Option<(Observed, Arc<[CertificateDer<'static>]>)>>,
}

impl OrgRoots {
    fn current(&self, host: &Arc<[CertificateDer<'static>]>) -> Arc<[CertificateDer<'static>]> {
        let observed = self.observe();
        let mut cached = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
        match &*cached {
            Some((seen, roots)) if *seen == observed => Arc::clone(roots),
            _ => {
                let roots: Arc<[CertificateDer<'static>]> =
                    host.iter().cloned().chain(self.load(&observed)).collect();
                *cached = Some((observed, Arc::clone(&roots)));
                roots
            }
        }
    }

    /// Stat every roots file without reading it. Temporaries start with `.`.
    fn observe(&self) -> Observed {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut observed: Observed = entries
            .flatten()
            .map(|entry| entry.file_name())
            .filter(|name| {
                let name = Path::new(name);
                name.extension().is_some_and(|ext| ext == "pem")
                    && !name.to_string_lossy().starts_with('.')
            })
            .map(|name| {
                let stamp = store::probe(&self.dir.join(&name), FileRole::OrgRoots)
                    .ok()
                    .flatten();
                (name, stamp)
            })
            .collect();
        observed.sort_by(|a, b| a.0.cmp(&b.0));
        observed
    }

    /// Every usable root in the observed files; a refused file is logged
    /// and skipped, so its organization admits nobody.
    fn load(&self, observed: &Observed) -> Vec<CertificateDer<'static>> {
        let mut roots = Vec::new();
        for (name, _) in observed {
            match store::read_stable(&self.dir.join(name), FileRole::OrgRoots) {
                Ok(Some((_, raw))) => roots.extend(
                    CertificateDer::pem_slice_iter(&raw)
                        .filter_map(Result::ok)
                        .filter(|root| rustls::RootCertStore::empty().add(root.clone()).is_ok()),
                ),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, "organization roots refused; no certificate they issued is admitted");
                }
            }
        }
        roots
    }
}
