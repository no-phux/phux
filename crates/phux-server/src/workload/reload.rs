//! Stat-generation hot reload of the workload registry (`workload-auth.md`
//! §7).
//!
//! Each lookup stats the file and re-reads only on change, never holding
//! the lock across I/O; a probe sequence number makes the newest probe win.
//! A broken or missing file installs (and caches) the empty snapshot, with
//! no fallback to a known-good generation; a transient failure denies that
//! lookup only and is not cached.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::store::{self, FileRole, Stamp};
use super::{WorkloadError, WorkloadRegistry};

/// What the last probe of the registry path saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Observed {
    /// No file: the empty snapshot.
    Missing,
    /// A file with this stamp (valid or not; the snapshot says which).
    File(Stamp),
    /// The path failed its integrity check: the empty snapshot.
    Refused,
}

impl Observed {
    const fn of(probed: &Result<Option<Stamp>, WorkloadError>) -> Self {
        match probed {
            Ok(None) => Self::Missing,
            Ok(Some(stamp)) => Self::File(*stamp),
            Err(_) => Self::Refused,
        }
    }
}

/// The result of loading a changed registry.
enum Loaded {
    /// A verdict cached until the file changes; `broken` for the empty
    /// snapshot of a bad file.
    Cache(Observed, Arc<WorkloadRegistry>, bool),
    /// A transient failure: admit no one now, retry on the next lookup.
    Transient,
}

struct Cached {
    observed: Observed,
    snapshot: Arc<WorkloadRegistry>,
    /// Whether `snapshot` stands in for a missing, insecure, or malformed
    /// file.
    broken: bool,
    /// Sequence number of the probe that produced `snapshot`.
    probe: u64,
}

impl Cached {
    fn observation(&self) -> RegistryObservation {
        if self.broken {
            RegistryObservation::Broken(BrokenRegistry(self.observed))
        } else {
            RegistryObservation::Loaded(Arc::clone(&self.snapshot))
        }
    }
}

/// What the registry file holds right now, as the live revocation watcher
/// needs to tell it (`workload-auth.md` §7).
#[derive(Debug, Clone)]
pub enum RegistryObservation {
    /// A valid registry: its verdicts apply at once.
    Loaded(Arc<WorkloadRegistry>),
    /// A missing, insecure, or malformed file (admits no one).
    Broken(BrokenRegistry),
    /// A read failed for a reason that may clear on its own.
    Transient,
}

/// One broken state of the registry file, comparable across observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokenRegistry(Observed);

/// A workload registry that observes replacement of its file without a
/// restart. Transports hold one and consult it on every connection.
pub struct ReloadingWorkloadRegistry {
    path: PathBuf,
    cached: Mutex<Cached>,
    probes: AtomicU64,
}

impl std::fmt::Debug for ReloadingWorkloadRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cached = self.lock();
        f.debug_struct("ReloadingWorkloadRegistry")
            .field("path", &self.path)
            .field("instance", &cached.snapshot.instance_id())
            .field("generation", &cached.snapshot.generation())
            .field("credentials", &cached.snapshot.len())
            .finish_non_exhaustive()
    }
}

impl ReloadingWorkloadRegistry {
    /// Load strictly (a bad file refuses startup, §8; missing is empty) and
    /// start tracking.
    ///
    /// # Errors
    ///
    /// Any [`WorkloadError`] reading or validating the registry.
    pub fn load(path: PathBuf) -> Result<Self, WorkloadError> {
        let (observed, snapshot) = read_snapshot(&path)?;
        Ok(Self {
            path,
            cached: Mutex::new(Cached {
                observed,
                snapshot: Arc::new(snapshot),
                broken: observed == Observed::Missing,
                probe: 0,
            }),
            probes: AtomicU64::new(0),
        })
    }

    /// Path of the tracked registry file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The snapshot for the registry file as it is now. A transient read
    /// failure admits no one: it is the empty snapshot, uncached.
    #[must_use]
    pub fn current(&self) -> Arc<WorkloadRegistry> {
        match self.observe() {
            RegistryObservation::Loaded(snapshot) => snapshot,
            RegistryObservation::Broken(_) | RegistryObservation::Transient => {
                Arc::new(WorkloadRegistry::empty())
            }
        }
    }

    /// The file's current state (valid, broken, or transient failure) for
    /// the revocation watcher (§7).
    #[must_use]
    pub fn observe(&self) -> RegistryObservation {
        let probe = self.probes.fetch_add(1, Ordering::SeqCst) + 1;
        let probed = store::probe(&self.path, FileRole::Registry);
        if let Some(observation) = self.cached_for(Observed::of(&probed)) {
            return observation;
        }
        match load_changed(&self.path, probed) {
            Loaded::Cache(observed, snapshot, broken) => {
                self.install(probe, observed, snapshot, broken)
            }
            Loaded::Transient => RegistryObservation::Transient,
        }
    }

    /// The active credential for a verified TLS leaf, with registry stamps.
    #[must_use]
    pub fn lookup_certificate(
        &self,
        certificate: &[u8],
    ) -> Option<crate::auth::AuthenticatedCredential> {
        let snapshot = self.current();
        snapshot
            .lookup_certificate(certificate)
            .map(|credential| credential.authenticated(&snapshot))
    }

    fn lock(&self) -> MutexGuard<'_, Cached> {
        self.cached.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn cached_for(&self, observed: Observed) -> Option<RegistryObservation> {
        let cached = self.lock();
        (cached.observed == observed).then(|| cached.observation())
    }

    /// Install a freshly loaded snapshot unless a later probe already did.
    fn install(
        &self,
        probe: u64,
        observed: Observed,
        snapshot: Arc<WorkloadRegistry>,
        broken: bool,
    ) -> RegistryObservation {
        let mut cached = self.lock();
        if probe > cached.probe {
            *cached = Cached {
                observed,
                snapshot,
                broken,
                probe,
            };
        }
        cached.observation()
    }
}

/// Failures that may clear on their own, so their empty snapshot is never
/// cached: an I/O error, or a file that kept changing while it was read.
const fn is_transient(error: &WorkloadError) -> bool {
    matches!(error, WorkloadError::Io(_) | WorkloadError::Unstable)
}

/// Produce the snapshot for a probe that differed from the cached one.
fn load_changed(path: &Path, probed: Result<Option<Stamp>, WorkloadError>) -> Loaded {
    let empty = || Arc::new(WorkloadRegistry::empty());
    match probed {
        Ok(None) => Loaded::Cache(Observed::Missing, empty(), true),
        Err(error) => {
            tracing::warn!(%error, "workload registry failed its integrity check; admitting no workload credential");
            Loaded::Cache(Observed::Refused, empty(), true)
        }
        Ok(Some(stamp)) => match read_snapshot(path) {
            Ok((observed, registry)) => {
                tracing::info!(
                    generation = registry.generation(),
                    credentials = registry.len(),
                    "workload registry reloaded"
                );
                let broken = observed == Observed::Missing;
                Loaded::Cache(observed, Arc::new(registry), broken)
            }
            Err(error) if is_transient(&error) => {
                tracing::warn!(%error, "workload registry could not be read; admitting no workload credential for this lookup and retrying on the next");
                Loaded::Transient
            }
            // Keyed by the probed stamp: the next change re-reads, and an
            // unchanged broken file stays empty without being re-parsed.
            Err(error) => {
                tracing::warn!(%error, "changed workload registry could not be loaded; admitting no workload credential");
                Loaded::Cache(Observed::File(stamp), empty(), true)
            }
        },
    }
}

fn read_snapshot(path: &Path) -> Result<(Observed, WorkloadRegistry), WorkloadError> {
    match store::read_stable(path, FileRole::Registry)? {
        None => Ok((Observed::Missing, WorkloadRegistry::empty())),
        Some((stamp, raw)) => Ok((Observed::File(stamp), WorkloadRegistry::from_bytes(&raw)?)),
    }
}
