//! Stat-generation hot reload of the workload registry (`workload-auth.md`
//! §7).
//!
//! Each lookup stats the registry file; an unchanged stamp reuses the cached
//! snapshot, and a changed one re-reads it. The snapshot lock is never held
//! across a filesystem call: the stat and the read both happen outside it,
//! and a probe sequence number decides which of two racing reloads installs,
//! so the newest probe always wins and a slow reader can never put an older
//! generation back.
//!
//! A malformed, insecure, or missing file installs the empty snapshot, and
//! that verdict is cached until the file changes. Nothing falls back to the
//! last known-good generation: a broken registry admits no one until a valid
//! one is written. A transient failure (an I/O error such as `EMFILE`, or a
//! read that never saw a stable file) admits no one for that lookup only and
//! is not cached, so the next lookup retries instead of locking everyone out
//! until the file happens to change.

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
    /// A verdict to cache under `Observed` until the file changes; `broken`
    /// when it is the empty snapshot of a missing, insecure, or malformed
    /// file rather than a valid registry.
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
    /// A missing, insecure, or malformed file, which admits no one. Two
    /// observations compare equal while the file stays in the same broken
    /// state.
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
    /// Load the registry and begin tracking its file.
    ///
    /// The initial load is strict: a malformed or insecure registry is an
    /// error, so a server configured for workload authority refuses to start
    /// on it (`workload-auth.md` §8). A missing file is the empty snapshot.
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

    /// What the registry file holds right now: a valid snapshot, a broken
    /// state (missing, insecure, or malformed), or a transient failure. The
    /// live revocation watcher applies a valid snapshot's verdicts at once
    /// and a broken state's empty snapshot only once it persists
    /// (`workload-auth.md` §7).
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

    /// Map a verified TLS leaf certificate to its active credential in the
    /// current snapshot, stamped with that snapshot's registry instance and
    /// generation.
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
