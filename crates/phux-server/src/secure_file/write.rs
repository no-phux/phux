//! The directory lock and atomic, owner-only replacement.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use super::{
    SecureFileError, Stamp, is_owned_regular_file, parent_of, refuse_production, validate_owner_dir,
};

/// Default workload file names; a write sweeps stale temporaries only for
/// these and the file it wrote.
const MANAGED_NAMES: [&str; 4] = [
    "workload-keys",
    "workload-ca.key",
    "workload-ca.pem",
    "enrollment-tickets",
];

/// Create (mode 0700) or open the directory that holds `path`, and check
/// that its owner alone can change it.
pub(crate) fn open_owner_dir(path: &Path) -> Result<File, SecureFileError> {
    // Every write (the lock, the CA pair, the registry, the credential
    // store) opens its directory here first, so this is the one production
    // guard for writes.
    refuse_production(path)?;
    let dir = parent_of(path);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let named = fs::symlink_metadata(dir)?;
    validate_owner_dir(&named)?;
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(dir)?;
    let opened = handle.metadata()?;
    validate_owner_dir(&opened)?;
    if !Stamp::of(&named).same_inode(&Stamp::of(&opened)) {
        return Err(SecureFileError::Unstable);
    }
    Ok(handle)
}

/// Holds an exclusive `flock` on a directory; unlocks on drop.
struct DirLock(File);

impl Drop for DirLock {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.0, rustix::fs::FlockOperation::Unlock);
    }
}

/// Proof that the caller holds one directory's lock (stale temporaries are
/// swept only there).
pub(crate) struct HeldLock<'a> {
    dir: &'a Path,
}

impl HeldLock<'_> {
    /// Whether `path` sits directly in the locked directory.
    fn covers(&self, path: &Path) -> bool {
        parent_of(path) == self.dir
    }
}

/// Run `operation` under an exclusive lock on `path`'s directory.
pub(crate) fn with_lock<T, E: From<SecureFileError>>(
    path: &Path,
    operation: impl FnOnce(&HeldLock<'_>) -> Result<T, E>,
) -> Result<T, E> {
    let lock = lock_dir(path)?;
    let result = operation(&HeldLock {
        dir: parent_of(path),
    });
    drop(lock);
    result
}

/// Open, `flock`, and re-check `path`'s directory.
fn lock_dir(path: &Path) -> Result<DirLock, SecureFileError> {
    let handle = open_owner_dir(path)?;
    rustix::fs::flock(&handle, rustix::fs::FlockOperation::LockExclusive)
        .map_err(io::Error::from)?;
    let lock = DirLock(handle);
    // The directory may have been renamed away between open and lock.
    let named = fs::symlink_metadata(parent_of(path))?;
    validate_owner_dir(&named)?;
    if !Stamp::of(&named).same_inode(&Stamp::of(&lock.0.metadata()?)) {
        return Err(SecureFileError::Unstable);
    }
    Ok(lock)
}

fn temp_path(path: &Path) -> Result<PathBuf, SecureFileError> {
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(|error| io::Error::other(error.to_string()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("secure-file");
    Ok(parent_of(path).join(format!(".{name}.{}.tmp", hex::encode(suffix))))
}

/// Whether an atomic replacement reached stable storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitOutcome {
    durable: bool,
}

impl CommitOutcome {
    /// Whether both file contents and the containing directory entry reached
    /// stable storage. A false result is already visible and must not be retried.
    #[must_use]
    pub const fn is_durable(self) -> bool {
        self.durable
    }
}

/// Write `bytes` to a fresh temporary at `tmp`, fsync it, and rename it
/// over `path`.
fn write_synced_and_rename(tmp: &Path, path: &Path, bytes: &[u8]) -> Result<(), SecureFileError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(tmp)?;
    file.write_all(bytes)?;
    #[cfg(test)]
    fault::interrupt(WriteFault::BeforeTempSync)?;
    file.sync_all()?;
    #[cfg(test)]
    fault::interrupt(WriteFault::AfterTempSyncBeforeRename)?;
    fs::rename(tmp, path)?;
    Ok(())
}

/// Fsync `path`'s directory; `false` (with a warning) when that fails.
fn sync_parent(path: &Path) -> bool {
    #[cfg(test)]
    if fault::interrupt(WriteFault::AfterRenameBeforeDirectorySync).is_err() {
        return false;
    }
    // The rename is already visible; a failed directory sync only means a
    // crash could lose it. Retrying would re-apply the mutation, so report.
    match File::open(parent_of(path)).and_then(|dir| dir.sync_all()) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "file replacement is visible but its directory sync failed; do not retry the mutation");
            false
        }
    }
}

/// Atomically replace `path` with `bytes` at mode 0600 (temp, fsync,
/// rename, directory fsync). A committed replacement whose directory sync
/// failed is visible, not durable, and reported as such.
pub(crate) fn atomic_replace(
    path: &Path,
    bytes: &[u8],
    held: &HeldLock<'_>,
) -> Result<CommitOutcome, SecureFileError> {
    let tmp = temp_path(path)?;
    if let Err(error) = write_synced_and_rename(&tmp, path, bytes) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    let durable = sync_parent(path);
    if held.covers(path) {
        remove_stale_temps(path);
    }
    Ok(CommitOutcome { durable })
}

/// A stage at which a test interrupts the next atomic replacement on its
/// thread.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WriteFault {
    /// After the temporary is written, before its fsync.
    BeforeTempSync,
    /// After the temporary's fsync, before the rename.
    AfterTempSyncBeforeRename,
    /// After the rename, before the directory fsync (visible, not durable).
    AfterRenameBeforeDirectorySync,
}

#[cfg(test)]
pub(crate) mod fault {
    use std::cell::Cell;

    use super::{SecureFileError, WriteFault};

    thread_local! {
        static ARMED: Cell<Option<WriteFault>> = const { Cell::new(None) };
    }

    /// Disarms the injected fault when dropped.
    pub(crate) struct Armed;

    impl Drop for Armed {
        fn drop(&mut self) {
            ARMED.set(None);
        }
    }

    /// Interrupt this thread's next replacement at `stage`.
    pub(crate) fn inject(stage: WriteFault) -> Armed {
        ARMED.set(Some(stage));
        Armed
    }

    /// Fail (once) if this thread armed a fault at `stage`.
    pub(super) fn interrupt(stage: WriteFault) -> Result<(), SecureFileError> {
        if ARMED.get() != Some(stage) {
            return Ok(());
        }
        ARMED.set(None);
        Err(
            std::io::Error::other(format!("injected atomic-write interruption at {stage:?}"))
                .into(),
        )
    }
}

/// Best effort: remove a crashed writer's temporaries beside `path` (only
/// the effective user's regular files, never following links).
fn remove_stale_temps(path: &Path) {
    let own = path.file_name().and_then(|name| name.to_str());
    let Ok(entries) = fs::read_dir(parent_of(path)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let stale = name.to_str().is_some_and(|name| is_stale_temp(name, own));
        if stale && is_owned_regular_file(&entry.path()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn is_stale_temp(name: &str, own: Option<&str>) -> bool {
    let Some(stem) = name
        .strip_prefix('.')
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    let Some((base, suffix)) = stem.rsplit_once('.') else {
        return false;
    };
    let random = suffix.len() == 16
        && suffix
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    random && (own == Some(base) || MANAGED_NAMES.contains(&base))
}
