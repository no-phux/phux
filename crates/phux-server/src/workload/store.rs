//! Owner-only, no-follow, lock-and-rename persistence for workload authority
//! files (`workload-auth.md` §2).
//!
//! Reads check owner control of the file and its directory, re-validate
//! across a no-follow open, and accept only a stat-stable read. Writes hold
//! an exclusive `flock` on the directory and commit via temp file, fsync,
//! rename, and directory fsync, sweeping stale temporaries only in the
//! locked directory. Ancestors must also be owner (or root) controlled.
//! File buffers are sized from metadata and scrubbed when not returned
//! (best effort). Diagnostics name a file's role, never its path.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use super::{WorkloadError, scrub};

/// Largest workload authority file this module reads.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Attempts at reading one stable generation before calling it unstable.
const STABLE_READ_ATTEMPTS: usize = 4;

/// Default workload file names; a write sweeps stale temporaries only for
/// these and the file it wrote.
const MANAGED_NAMES: [&str; 3] = ["workload-keys", "workload-ca.key", "workload-ca.pem"];

/// The role a file plays: the only name a diagnostic gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FileRole {
    /// `<state-dir>/workload-keys`.
    Registry,
    /// `<state-dir>/workload-ca.pem` (public).
    CaCertificate,
    /// `<state-dir>/workload-ca.key` (secret).
    CaPrivateKey,
}

impl FileRole {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Registry => "registry",
            Self::CaCertificate => "CA certificate",
            Self::CaPrivateKey => "CA private key",
        }
    }

    /// Mode bits this role may not carry (the public CA cert only refuses
    /// group/world write).
    const fn forbidden_mode_bits(self) -> u32 {
        match self {
            Self::CaCertificate => 0o022,
            Self::Registry | Self::CaPrivateKey => 0o077,
        }
    }
}

const fn insecure(file: &'static str, reason: &'static str) -> WorkloadError {
    WorkloadError::Insecure { file, reason }
}

/// The identity and content generation of one file, from `lstat`/`fstat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mode: u32,
    uid: u32,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
}

impl Stamp {
    fn of(meta: &fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mode: meta.mode(),
            uid: meta.uid(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
            ctime: meta.ctime(),
            ctime_nsec: meta.ctime_nsec(),
        }
    }

    const fn same_inode(&self, other: &Self) -> bool {
        self.dev == other.dev && self.ino == other.ino
    }
}

fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn validate_owner_file(meta: &fs::Metadata, role: FileRole) -> Result<(), WorkloadError> {
    let file = role.as_str();
    if meta.file_type().is_symlink() {
        return Err(insecure(file, "is a symbolic link"));
    }
    if !meta.file_type().is_file() {
        return Err(insecure(file, "is not a regular file"));
    }
    if meta.uid() != effective_uid() {
        return Err(insecure(file, "is not owned by the effective user"));
    }
    if meta.mode() & role.forbidden_mode_bits() != 0 {
        return Err(insecure(
            file,
            "grants group or world access; use mode 0600",
        ));
    }
    if meta.mode() & 0o400 == 0 {
        return Err(insecure(file, "is not readable by its owner"));
    }
    Ok(())
}

/// Stat `path` without following links and require owner-only control of it
/// and its directory. `Ok(None)` if absent.
pub(super) fn probe(path: &Path, role: FileRole) -> Result<Option<Stamp>, WorkloadError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    validate_owner_dir(&fs::symlink_metadata(parent_of(path))?)?;
    validate_owner_file(&meta, role)?;
    Ok(Some(Stamp::of(&meta)))
}

/// Read `path` once: `lstat`, `O_NOFOLLOW` open, `fstat` of the same inode,
/// a buffer sized from the recorded length. Any size change is unstable and
/// the buffer is scrubbed.
fn read_owner_file(path: &Path, role: FileRole) -> Result<Option<(Stamp, Vec<u8>)>, WorkloadError> {
    // The CA key is the one secret here: a dev build holding production's
    // could issue credentials the production server trusts.
    if matches!(role, FileRole::CaPrivateKey) {
        refuse_production(path)?;
    }
    let Some(before) = probe(path, role)? else {
        return Ok(None);
    };
    #[cfg(test)]
    fault::take()?;
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(WorkloadError::Unstable);
        }
        Err(error) => return Err(error.into()),
    };
    let opened = file.metadata()?;
    validate_owner_file(&opened, role)?;
    let stamp = Stamp::of(&opened);
    if !stamp.same_inode(&before) {
        return Err(WorkloadError::Unstable);
    }
    let expected = usize::try_from(stamp.len)
        .ok()
        .filter(|_| stamp.len <= MAX_FILE_BYTES)
        .ok_or_else(|| insecure(role.as_str(), "exceeds the 4 MiB size limit"))?;
    // Capacity for the recorded length plus the one byte that would reveal
    // growth: the read never outgrows the buffer, so nothing reallocates.
    let mut raw = Vec::with_capacity(expected + 1);
    let read = Read::take(&mut file, stamp.len + 1).read_to_end(&mut raw);
    if read.is_err() || raw.len() != expected {
        scrub(&mut raw);
        return Err(read
            .err()
            .map_or(WorkloadError::Unstable, WorkloadError::from));
    }
    Ok(Some((stamp, raw)))
}

/// Read one stat-stable generation of `path`, retrying a bounded number of
/// times. `Ok(None)` if absent.
pub(super) fn read_stable(
    path: &Path,
    role: FileRole,
) -> Result<Option<(Stamp, Vec<u8>)>, WorkloadError> {
    for _ in 0..STABLE_READ_ATTEMPTS {
        let read = match read_owner_file(path, role) {
            Err(WorkloadError::Unstable) => continue,
            other => other?,
        };
        let after = match probe(path, role) {
            Ok(after) => after,
            Err(error) => {
                discard(read);
                return Err(error);
            }
        };
        match (read, after) {
            (None, None) => return Ok(None),
            (Some((stamp, raw)), Some(after)) if stamp == after => return Ok(Some((stamp, raw))),
            (read, _) => discard(read),
        }
    }
    Err(WorkloadError::Unstable)
}

/// Drop a read that did not become the answer, scrubbing its bytes first.
fn discard(read: Option<(Stamp, Vec<u8>)>) {
    if let Some((_, mut raw)) = read {
        scrub(&mut raw);
    }
}

/// The directory holding `path` (`.` for a bare file name).
pub(super) fn parent_of(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn validate_owner_dir(meta: &fs::Metadata) -> Result<(), WorkloadError> {
    const DIR: &str = "state directory";
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        return Err(insecure(DIR, "is not a directory"));
    }
    if meta.uid() != effective_uid() {
        return Err(insecure(DIR, "is not owned by the effective user"));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(insecure(DIR, "is writable by group or world"));
    }
    if meta.mode() & 0o700 != 0o700 {
        return Err(insecure(DIR, "is not fully accessible to its owner"));
    }
    Ok(())
}

/// Create (mode 0700) or open the directory that holds `path`, and check
/// that its owner alone can change it.
pub(super) fn open_owner_dir(path: &Path) -> Result<File, WorkloadError> {
    // Every write (the lock, the CA pair, the registry) opens its directory
    // here first, so this is the store's one production guard.
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
        return Err(WorkloadError::Unstable);
    }
    Ok(handle)
}

fn refuse_production(path: &Path) -> Result<(), WorkloadError> {
    phux_config::production::refuse_dev_on_production_state(path)
        .map_err(WorkloadError::ProductionState)
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
pub(super) struct HeldLock<'a> {
    dir: &'a Path,
}

impl HeldLock<'_> {
    /// Whether `path` sits directly in the locked directory.
    fn covers(&self, path: &Path) -> bool {
        parent_of(path) == self.dir
    }
}

/// Run `operation` under an exclusive lock on `path`'s directory.
pub(super) fn with_lock<T>(
    path: &Path,
    operation: impl FnOnce(&HeldLock<'_>) -> Result<T, WorkloadError>,
) -> Result<T, WorkloadError> {
    let handle = open_owner_dir(path)?;
    rustix::fs::flock(&handle, rustix::fs::FlockOperation::LockExclusive)
        .map_err(io::Error::from)?;
    let lock = DirLock(handle);
    // The directory may have been renamed away between open and lock.
    let named = fs::symlink_metadata(parent_of(path))?;
    validate_owner_dir(&named)?;
    if !Stamp::of(&named).same_inode(&Stamp::of(&lock.0.metadata()?)) {
        return Err(WorkloadError::Unstable);
    }
    operation(&HeldLock {
        dir: parent_of(path),
    })
}

fn temp_path(path: &Path) -> Result<PathBuf, WorkloadError> {
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(|error| io::Error::other(error.to_string()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workload");
    Ok(parent_of(path).join(format!(".{name}.{}.tmp", hex::encode(suffix))))
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), WorkloadError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Atomically replace `path` with `bytes` at mode 0600 (temp, fsync,
/// rename, directory fsync).
pub(super) fn atomic_replace(
    path: &Path,
    bytes: &[u8],
    held: &HeldLock<'_>,
) -> Result<(), WorkloadError> {
    let tmp = temp_path(path)?;
    let committed = write_synced(&tmp, bytes).and_then(|()| Ok(fs::rename(&tmp, path)?));
    if let Err(error) = committed {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    // The rename is already visible; a failed directory sync only means a
    // crash could lose it. Retrying would re-apply the mutation, so report.
    if let Err(error) = File::open(parent_of(path)).and_then(|dir| dir.sync_all()) {
        tracing::warn!(%error, "workload authority file replaced, but its directory sync failed");
    }
    if held.covers(path) {
        remove_stale_temps(path);
    }
    Ok(())
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

pub(super) fn is_owned_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.file_type().is_file() && meta.uid() == effective_uid())
}

/// A test-only fault: make the next file read on this thread fail as a
/// transient I/O error would (`EMFILE`), before anything is opened.
#[cfg(test)]
pub(super) mod fault {
    use std::cell::Cell;

    thread_local! {
        static FAIL_NEXT_READ: Cell<bool> = const { Cell::new(false) };
    }

    /// Arm the fault for the next read on this thread.
    pub(in crate::workload) fn fail_next_read() {
        FAIL_NEXT_READ.with(|armed| armed.set(true));
    }

    pub(super) fn take() -> Result<(), super::WorkloadError> {
        if FAIL_NEXT_READ.with(|armed| armed.replace(false)) {
            return Err(std::io::Error::from_raw_os_error(nix::libc::EMFILE).into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn owner_only(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn insecure_types_and_modes_are_refused_by_role_without_naming_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret-location.key");
        std::fs::write(&file, b"x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = read_stable(&file, FileRole::CaPrivateKey).unwrap_err();
        assert!(matches!(error, WorkloadError::Insecure { .. }), "{error}");
        assert!(!error.to_string().contains("secret-location"), "{error}");
        // The public certificate tolerates world read, never world write.
        assert!(read_stable(&file, FileRole::CaCertificate).is_ok());

        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(matches!(
            read_stable(&link, FileRole::CaCertificate),
            Err(WorkloadError::Insecure { .. })
        ));
        assert!(matches!(
            read_stable(dir.path(), FileRole::Registry),
            Err(WorkloadError::Insecure { .. })
        ));
    }

    #[test]
    fn replacement_is_owner_only_and_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workload-keys");
        owner_only(&path, b"old");
        with_lock(&path, |held| atomic_replace(&path, b"new", held)).unwrap();
        let (_, raw) = read_stable(&path, FileRole::Registry).unwrap().unwrap();
        assert_eq!(raw, b"new");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from("workload-keys")]);
    }

    #[test]
    fn a_group_writable_directory_is_not_an_owner_controlled_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o775)).unwrap();
        let path = dir.path().join("workload-keys");
        assert!(matches!(
            with_lock(&path, |_| Ok(())),
            Err(WorkloadError::Insecure { .. })
        ));
    }

    /// A reader refuses an owner-only file in a directory others can write:
    /// they could swap it between two reads.
    #[test]
    fn a_reader_refuses_a_file_in_a_shared_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workload-keys");
        owner_only(&path, b"{}");
        assert!(read_stable(&path, FileRole::Registry).is_ok());
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(
            probe(&path, FileRole::Registry),
            Err(WorkloadError::Insecure {
                file: "state directory",
                ..
            })
        ));
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn a_write_removes_stale_workload_temporaries_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workload-keys");
        let stale = [
            ".workload-ca.key.0123456789abcdef.tmp",
            ".workload-keys.fedcba9876543210.tmp",
        ];
        let kept = [
            ".phux-tokens.json.0123456789abcdef.tmp",
            ".workload-keys.notrandom.tmp",
            "workload-ca.pem",
        ];
        for name in stale.iter().chain(&kept) {
            owner_only(&dir.path().join(name), b"x");
        }
        with_lock(&path, |held| atomic_replace(&path, b"{}", held)).unwrap();
        for name in stale {
            assert!(!dir.path().join(name).exists(), "{name} was not removed");
        }
        for name in kept {
            assert!(dir.path().join(name).exists(), "{name} was removed");
        }
    }

    /// Temporaries in a directory the caller did not lock are left alone.
    #[test]
    fn a_write_sweeps_only_the_directory_whose_lock_it_holds() {
        let locked = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let key = locked.path().join("workload-ca.key");
        let cert = elsewhere.path().join("workload-ca.pem");
        let foreign = elsewhere.path().join(".workload-keys.0123456789abcdef.tmp");
        let stale = locked.path().join(".workload-ca.key.fedcba9876543210.tmp");
        owner_only(&foreign, b"x");
        owner_only(&stale, b"x");
        with_lock(&key, |held| {
            atomic_replace(&cert, b"cert", held)?;
            atomic_replace(&key, b"key", held)
        })
        .unwrap();
        assert!(foreign.exists(), "a temp outside the held lock was swept");
        assert!(!stale.exists(), "the held directory was not swept");
    }

    #[test]
    fn the_injected_fault_fails_one_read_as_io() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workload-keys");
        owner_only(&path, b"{}");
        fault::fail_next_read();
        assert!(matches!(
            read_stable(&path, FileRole::Registry),
            Err(WorkloadError::Io(_))
        ));
        assert!(read_stable(&path, FileRole::Registry).is_ok());
    }
}
