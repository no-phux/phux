//! A credential store that re-reads its file when the generation changes.

use std::fs::{self};
use std::io::{self};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};

use chrono::Utc;

use super::{
    AuthError, AuthenticatedCredential, STABLE_READ_ATTEMPTS, Standing, TokenStore,
    record_credential_seen, validate_store_metadata,
};

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
struct Stamp {
    mtime: Option<std::time::SystemTime>,
    len: u64,
    dev: u64,
    ino: u64,
    uid: u32,
    mode: u32,
    ctime: i64,
    ctime_nsec: i64,
}

impl Stamp {
    fn probe(path: &Path) -> Result<Option<Self>, AuthError> {
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if meta.file_type().is_symlink() {
            return Err(AuthError::InsecureStore(format!(
                "{} is a symbolic link",
                path.display()
            )));
        }
        validate_store_metadata(path, &meta)?;
        Ok(Some(Self {
            mtime: meta.modified().ok(),
            len: meta.len(),
            dev: meta.dev(),
            ino: meta.ino(),
            uid: meta.uid(),
            mode: meta.mode(),
            ctime: meta.ctime(),
            ctime_nsec: meta.ctime_nsec(),
        }))
    }
}

struct Cached {
    stamp: Option<Stamp>,
    store: TokenStore,
    reloads: u64,
}

/// A token-store refresh that failed, and the stage it failed at.
struct RefreshFailure {
    stage: &'static str,
    error: AuthError,
}

/// Fail closed: the cache holds the empty store, never the prior generation.
fn fail_closed(cached: &mut Cached, stage: &'static str, error: AuthError) -> RefreshFailure {
    cached.stamp = None;
    cached.store = TokenStore::default();
    RefreshFailure { stage, error }
}

/// Why a token store gives the live revocation watcher no verdict.
#[derive(Debug, Clone, Copy)]
enum StoreCondition {
    /// Missing, or holding no credentials.
    Empty,
    /// Failed its ownership or mode check.
    Insecure,
    /// Does not parse (or is a retired legacy store).
    Unparseable,
    /// Could not be read, or kept changing while read.
    Unreadable,
}

impl StoreCondition {
    const fn of(error: &AuthError) -> Self {
        match error {
            AuthError::InsecureStore(_) | AuthError::ProductionStore(_) => Self::Insecure,
            AuthError::Io(_) | AuthError::UnstableStore => Self::Unreadable,
            _ => Self::Unparseable,
        }
    }

    const fn remedy(self) -> &'static str {
        match self {
            Self::Empty => {
                "the credential store is missing or holds no credentials; pair devices again with `phux pair`"
            }
            Self::Insecure => {
                "the credential store failed its integrity check; make it a regular file owned by this user with mode 0600"
            }
            Self::Unparseable => {
                "the credential store does not parse; restore it, or remove it and pair devices again with `phux pair`"
            }
            Self::Unreadable => "the credential store could not be read; retrying",
        }
    }

    fn warn(self, path: &Path) {
        if !no_verdict_warning_due() {
            return;
        }
        let remedy = self.remedy();
        tracing::warn!(
            path = %path.display(),
            condition = ?self,
            "{remedy}: live paired sessions keep running, and new connections are refused until it is fixed"
        );
    }
}

/// Seconds between two no-verdict warnings, process-wide: the watcher polls
/// every 250 ms, and one line a minute names the problem.
const NO_VERDICT_WARN_INTERVAL_SECS: i64 = 60;
static LAST_NO_VERDICT_WARNING: AtomicI64 = AtomicI64::new(i64::MIN);

fn no_verdict_warning_due() -> bool {
    let now = Utc::now().timestamp();
    let last = LAST_NO_VERDICT_WARNING.load(Ordering::Relaxed);
    now.saturating_sub(last) >= NO_VERDICT_WARN_INTERVAL_SECS
        && LAST_NO_VERDICT_WARNING
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
}

/// A last-known-good snapshot that re-reads after each atomic file generation.
pub struct ReloadingTokenStore {
    path: PathBuf,
    cached: std::sync::Mutex<Cached>,
}

#[allow(
    clippy::missing_fields_in_debug,
    reason = "the cached field contains credential verifiers and is deliberately redacted"
)]
impl std::fmt::Debug for ReloadingTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReloadingTokenStore")
            .field("path", &self.path)
            .field("credentials", &self.len())
            .finish()
    }
}

impl ReloadingTokenStore {
    const fn from_snapshot(path: PathBuf, stamp: Option<Stamp>, store: TokenStore) -> Self {
        Self {
            path,
            cached: std::sync::Mutex::new(Cached {
                stamp,
                store,
                reloads: 0,
            }),
        }
    }

    /// Load the current snapshot and begin tracking its file generation.
    pub fn load(path: PathBuf) -> Result<Self, AuthError> {
        Self::load_observed(path, |_| {})
    }

    /// Load the current snapshot, or start empty and adopt the file once it
    /// loads. Listeners use this so a store that will not load at boot
    /// (legacy, mid-repair, wrong mode) refuses every authentication instead
    /// of keeping the remote surface down until restart. Returns the boot
    /// error for reporting.
    #[must_use]
    pub fn load_deferred(path: PathBuf) -> (Self, Option<AuthError>) {
        match Self::load_observed(path.clone(), |_| {}) {
            Ok(store) => (store, None),
            Err(error) => (
                Self::from_snapshot(path, None, TokenStore::default()),
                Some(error),
            ),
        }
    }

    pub(super) fn load_observed(
        path: PathBuf,
        after_load: impl FnMut(usize),
    ) -> Result<Self, AuthError> {
        let (stamp, store) = stable_load_observed(&path, after_load)?;
        Ok(Self::from_snapshot(path, stamp, store))
    }

    /// Path of the tracked credential store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn with_current<T>(&self, f: impl FnOnce(&TokenStore) -> T) -> T {
        self.with_current_observed(|_| {}, f)
    }

    pub(super) fn with_current_observed<T>(
        &self,
        after_load: impl FnMut(usize),
        f: impl FnOnce(&TokenStore) -> T,
    ) -> T {
        let mut cached = self.lock();
        // A failed refresh leaves the empty store: admission fails closed.
        if let Err(failure) = self.refresh(&mut cached, after_load) {
            let stage = failure.stage;
            tracing::warn!(path = %self.path.display(), error = %failure.error, "{stage}; denying authentication");
        }
        f(&cached.store)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Cached> {
        self.cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Refresh `cached` to the file's generation; on failure it holds the
    /// empty store and the error is returned.
    fn refresh(
        &self,
        cached: &mut Cached,
        after_load: impl FnMut(usize),
    ) -> Result<(), RefreshFailure> {
        let stamp = match Stamp::probe(&self.path) {
            Ok(stamp) => stamp,
            Err(error) => {
                return Err(fail_closed(
                    cached,
                    "credential store integrity check failed",
                    error,
                ));
            }
        };
        if stamp == cached.stamp {
            return Ok(());
        }
        match stable_load_observed(&self.path, after_load) {
            Ok((stamp, store)) => {
                cached.stamp = stamp;
                cached.store = store;
                cached.reloads = cached.reloads.saturating_add(1);
                Ok(())
            }
            Err(error) => Err(fail_closed(
                cached,
                "changed credential store could not be loaded",
                error,
            )),
        }
    }

    /// The store for the live revocation watcher (§7): a copy of the
    /// current generation only if it loaded cleanly with at least one
    /// credential. Anything else is no verdict (warned at most once a
    /// minute); new admissions still fail closed.
    #[must_use]
    pub fn watch_snapshot(&self) -> Option<TokenStore> {
        let mut cached = self.lock();
        let condition = match self.refresh(&mut cached, |_| {}) {
            Ok(()) if !cached.store.is_empty() => return Some(cached.store.clone()),
            Ok(()) => StoreCondition::Empty,
            Err(failure) => StoreCondition::of(&failure.error),
        };
        drop(cached);
        condition.warn(&self.path);
        None
    }

    /// The standing of credential `id` at `generation` in the current
    /// generation, or `None` when the store gives no verdict (see
    /// [`Self::watch_snapshot`]).
    #[must_use]
    pub fn standing(&self, id: &str, generation: u64) -> Option<Standing> {
        self.watch_snapshot()
            .map(|store| store.standing_at(id, generation, Utc::now()))
    }

    /// Authenticate against the current readable generation.
    #[must_use]
    pub fn authenticate(&self, presented: &[u8]) -> Option<AuthenticatedCredential> {
        self.with_current(|store| store.authenticate(presented))
    }

    /// Authenticate and record `last_seen` (admission path);
    /// [`Self::authenticate`] stays read-only.
    #[must_use]
    pub fn authenticate_and_touch(&self, presented: &[u8]) -> Option<AuthenticatedCredential> {
        let authenticated = self.authenticate(presented)?;
        record_credential_seen(&self.path, &authenticated.id);
        Some(authenticated)
    }

    /// Whether a bearer secret authenticates against the current generation.
    #[must_use]
    pub fn verify(&self, presented: &[u8]) -> bool {
        self.authenticate(presented).is_some()
    }

    /// Number of credential generations in the current snapshot.
    #[must_use]
    pub fn len(&self) -> usize {
        self.with_current(TokenStore::len)
    }

    /// Whether the current snapshot contains no credentials.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.with_current(TokenStore::is_empty)
    }

    #[cfg(test)]
    pub(super) fn reloads(&self) -> u64 {
        self.cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reloads
    }

    #[cfg(test)]
    pub(super) fn cached_is_empty(&self) -> bool {
        self.cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .store
            .is_empty()
    }
}

fn stable_load_observed(
    path: &Path,
    mut after_load: impl FnMut(usize),
) -> Result<(Option<Stamp>, TokenStore), AuthError> {
    for attempt in 0..STABLE_READ_ATTEMPTS {
        let before = Stamp::probe(path)?;
        let loaded = TokenStore::load(path);
        after_load(attempt);
        let after = Stamp::probe(path)?;
        if before == after {
            return loaded.map(|store| (after, store));
        }
    }
    Err(AuthError::UnstableStore)
}
