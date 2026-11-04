//! Verify, unpack, and replace — the half of `phux update` that touches the
//! filesystem.
//!
//! 1. The checksum gates everything: nothing is unpacked or replaced unless the
//!    published digest matches the file on disk.
//! 2. Nothing downloaded is executed to decide whether to install it; the
//!    archive's member list and extracted tree are both validated.
//! 3. Publication is a recoverable transaction: a persistent lock serializes
//!    publishers, the old pair is fsynced into a sibling journal before any
//!    destination changes, every rename is followed by a directory fsync, and
//!    renaming the journal into the rollback directory is the durable commit.
//!    Later runs recover an interrupted pre-commit pair first.
//!
//! Permissions come from the file being replaced, not the archive, so a
//! restrictive mode survives and a smuggled setuid bit does not.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use super::UpdateError;

mod transaction;
pub(crate) use transaction::rollback;
use transaction::{prepare_transaction, recover_interrupted_transaction, sync_directory};

/// The binaries a phux release tarball ships, in replacement order: `phux`
/// moves last, since two renames cannot be one atomic step.
pub(crate) const RELEASE_BINARIES: &[&str] = &["phux-mcp", "phux"];

/// Non-executable members a release tarball may carry: the pre-relicense
/// (<= v0.35.0) and current license layouts, as `scripts/install.sh` accepts.
const RELEASE_DOCS: &[&str] = &[
    "README.md",
    "LICENSE-MIT",
    "LICENSE-APACHE",
    "LICENSE",
    "NOTICE",
    "THIRD-PARTY-NOTICES.md",
];

/// The directory, inside the install's bin directory, that holds the previous
/// binaries (same filesystem, so a rollback is a rename).
pub(crate) const BACKUP_DIR: &str = ".phux-update-backup";

/// The manifest written beside the saved binaries.
const BACKUP_MANIFEST: &str = "manifest.json";

/// Stable names used by the crash-recovery state machine.
const PREPARING_DIR: &str = ".phux-update-transaction.preparing";
const TRANSACTION_DIR: &str = ".phux-update-transaction";
const PREVIOUS_BACKUP_DIR: &str = ".phux-update-backup.previous";
const ROLLBACK_COMMITTED_DIR: &str = ".phux-update-rollback-committed";
const UPDATE_LOCK: &str = ".phux-update.lock";

/// Compute the SHA-256 of a file as lowercase hex.
pub(crate) fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

/// Lowercase hex, without pulling in a dependency for sixteen characters.
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Verify `archive` against a published digest (the channel manifest's, or a
/// sidecar's), returning it. This is the trust anchor: everything downstream
/// runs only on `Ok`.
pub(crate) fn verify_digest(
    archive: &Path,
    expected: &str,
    archive_name: &str,
) -> Result<String, UpdateError> {
    let expected = expected.to_ascii_lowercase();
    let actual = sha256_file(archive)
        .map_err(|err| UpdateError::Checksum(format!("could not hash the download: {err}")))?;
    if actual == expected {
        Ok(actual)
    } else {
        Err(UpdateError::ChecksumMismatch {
            expected,
            actual,
            archive: archive_name.to_owned(),
        })
    }
}

/// The exact set of members a `phux-<tag>-<target>.tar.gz` may contain.
fn allowed_members(stage: &str) -> BTreeSet<String> {
    let mut allowed = BTreeSet::new();
    allowed.insert(format!("{stage}/"));
    allowed.insert(stage.to_owned());
    for name in RELEASE_BINARIES.iter().chain(RELEASE_DOCS) {
        allowed.insert(format!("{stage}/{name}"));
    }
    allowed
}

/// Unpack a verified archive into `into`, returning the staged directory. The
/// listing is checked before extraction (paths, extra members) and the tree
/// after it (symlinks, hard links, device nodes).
pub(crate) fn unpack_verified(
    archive: &Path,
    stage: &str,
    into: &Path,
) -> Result<PathBuf, UpdateError> {
    let listing = tar(&["-tzf", &archive.to_string_lossy()])?;
    let allowed = allowed_members(stage);
    let mut saw_binary = false;
    for member in listing.lines().map(str::trim).filter(|m| !m.is_empty()) {
        if !allowed.contains(member) {
            return Err(UpdateError::Archive(format!(
                "unexpected member `{member}` in {}",
                archive.display()
            )));
        }
        if member == format!("{stage}/phux") {
            saw_binary = true;
        }
    }
    if !saw_binary {
        return Err(UpdateError::Archive(format!(
            "{} does not contain {stage}/phux",
            archive.display()
        )));
    }

    tar(&[
        "-xzf",
        &archive.to_string_lossy(),
        "-C",
        &into.to_string_lossy(),
    ])?;

    let staged = into.join(stage);
    validate_extracted(&staged, stage)?;
    Ok(staged)
}

/// Check the tree `tar` actually produced.
fn validate_extracted(staged: &Path, stage: &str) -> Result<(), UpdateError> {
    let meta = fs::symlink_metadata(staged).map_err(|err| {
        UpdateError::Archive(format!("{} is unreadable: {err}", staged.display()))
    })?;
    if !meta.is_dir() {
        return Err(UpdateError::Archive(format!(
            "{} is not a directory",
            staged.display()
        )));
    }
    let entries = fs::read_dir(staged).map_err(|err| {
        UpdateError::Archive(format!("{} is unreadable: {err}", staged.display()))
    })?;
    let allowed = allowed_members(stage);
    for entry in entries {
        let entry =
            entry.map_err(|err| UpdateError::Archive(format!("unpack directory error: {err}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !allowed.contains(&format!("{stage}/{name}")) {
            return Err(UpdateError::Archive(format!(
                "unexpected extracted member `{name}`"
            )));
        }
        let meta = fs::symlink_metadata(entry.path()).map_err(|err| {
            UpdateError::Archive(format!("{} is unreadable: {err}", entry.path().display()))
        })?;
        if meta.file_type().is_symlink() {
            return Err(UpdateError::Archive(format!(
                "extracted member `{name}` is a symlink"
            )));
        }
        if !meta.is_file() {
            return Err(UpdateError::Archive(format!(
                "extracted member `{name}` is not a regular file"
            )));
        }
    }
    Ok(())
}

/// Run `tar` with `args`.
fn tar(args: &[&str]) -> Result<String, UpdateError> {
    let output = Command::new("tar")
        .args(args)
        .output()
        .map_err(|err| UpdateError::Archive(format!("could not run `tar`: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(UpdateError::Archive(format!(
            "`tar` exited with {}: {}",
            output.status,
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A scratch directory beside the target, removed on drop. Sharing the
/// target's filesystem is what makes the final `rename` atomic.
#[derive(Debug)]
pub(crate) struct Staging {
    path: PathBuf,
}

impl Staging {
    /// Create a fresh staging directory beside the binaries in `bin_dir`.
    pub(crate) fn create(bin_dir: &Path) -> Result<Self, UpdateError> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let path = bin_dir.join(format!(".phux-update-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).map_err(|err| {
            UpdateError::Install(format!(
                "could not create a staging directory at {}: {err}\n\
                 {} must be writable by this user for phux to update in place",
                path.display(),
                bin_dir.display()
            ))
        })?;
        Ok(Self { path })
    }

    /// The staging directory.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The outcome of a successful replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Replaced {
    /// The binaries that were replaced, in the order they moved.
    pub(crate) binaries: Vec<String>,
    /// Where the previous binaries were saved.
    pub(crate) backup: PathBuf,
}

/// Fault-injection boundaries in an install publication transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallCheckpoint {
    Journal,
    FirstBinaryVisible,
    FirstBinary,
    AllBinariesVisible,
    AllBinaries,
    PreviousBackupVisible,
    PreviousBackup,
    CommitVisible,
    Commit,
}

/// Fault-injection boundaries in a rollback transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RollbackCheckpoint {
    Journal,
    FirstBinaryVisible,
    FirstBinary,
    AllBinariesVisible,
    AllBinaries,
    CommitVisible,
    Commit,
}

/// The persistent lock serializing update and rollback publication.
struct UpdateLock {
    file: fs::File,
}

impl UpdateLock {
    fn acquire(bin_dir: &Path) -> Result<Self, UpdateError> {
        let path = bin_dir.join(UPDATE_LOCK);
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
            .open(&path)
            .map_err(|err| {
                UpdateError::Install(format!(
                    "could not open update lock {}: {err}",
                    path.display()
                ))
            })?;
        if !file
            .metadata()
            .map_err(|err| {
                UpdateError::Install(format!(
                    "could not inspect update lock {}: {err}",
                    path.display()
                ))
            })?
            .is_file()
        {
            return Err(UpdateError::Install(format!(
                "update lock {} is not a regular file",
                path.display()
            )));
        }
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive).map_err(|err| {
            UpdateError::Install(format!("could not lock {}: {err}", path.display()))
        })?;
        let lock = Self { file };
        recover_interrupted_transaction(bin_dir)?;
        Ok(lock)
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.file, rustix::fs::FlockOperation::Unlock);
    }
}

/// Atomically replace the release binaries in `bin_dir` from `staged`.
/// `phux-mcp` is replaced alongside `phux` when present: the release is the
/// compatibility unit (ADR-0071).
pub(crate) fn replace_binaries(
    bin_dir: &Path,
    staged: &Path,
    previous_version: &str,
) -> Result<Replaced, UpdateError> {
    replace_binaries_with_checkpoint(bin_dir, staged, previous_version, |_| {})
}

fn replace_binaries_with_checkpoint(
    bin_dir: &Path,
    staged: &Path,
    previous_version: &str,
    mut checkpoint: impl FnMut(InstallCheckpoint),
) -> Result<Replaced, UpdateError> {
    let _lock = UpdateLock::acquire(bin_dir)?;
    let targets: Vec<&str> = RELEASE_BINARIES
        .iter()
        .copied()
        .filter(|name| *name == "phux" || bin_dir.join(name).exists())
        .collect();

    let transaction = prepare_transaction(bin_dir, &targets, previous_version)?;
    checkpoint(InstallCheckpoint::Journal);

    let publish = (|| {
        let moved = publish_binaries(
            bin_dir,
            &targets,
            |name| install_staged_binary(staged, bin_dir, name),
            |step| checkpoint(step.into()),
        )?;
        checkpoint(InstallCheckpoint::AllBinaries);
        let backup = commit_install(bin_dir, &transaction, &mut checkpoint)?;
        Ok(Replaced {
            binaries: moved,
            backup,
        })
    })();

    if publish.is_err()
        && let Err(recovery) = recover_interrupted_transaction(bin_dir)
    {
        return Err(UpdateError::Install(format!(
            "update failed and recovery also failed: {recovery}"
        )));
    }
    publish
}

/// A per-binary crash boundary inside [`publish_binaries`].
#[derive(Debug, Clone, Copy)]
enum BinaryStep {
    /// The first of several binaries is renamed into place.
    FirstVisible,
    /// That first rename is durable.
    First,
    /// The last binary is renamed into place.
    AllVisible,
}

impl From<BinaryStep> for InstallCheckpoint {
    fn from(step: BinaryStep) -> Self {
        match step {
            BinaryStep::FirstVisible => Self::FirstBinaryVisible,
            BinaryStep::First => Self::FirstBinary,
            BinaryStep::AllVisible => Self::AllBinariesVisible,
        }
    }
}

impl From<BinaryStep> for RollbackCheckpoint {
    fn from(step: BinaryStep) -> Self {
        match step {
            BinaryStep::FirstVisible => Self::FirstBinaryVisible,
            BinaryStep::First => Self::FirstBinary,
            BinaryStep::AllVisible => Self::AllBinariesVisible,
        }
    }
}

/// Rename each binary into place with `publish_one`, making `bin_dir`
/// durable after each, and report the per-binary crash boundaries to `step`.
/// Returns the names published, in order.
fn publish_binaries<S: AsRef<str>>(
    bin_dir: &Path,
    names: &[S],
    mut publish_one: impl FnMut(&str) -> Result<(), UpdateError>,
    mut step: impl FnMut(BinaryStep),
) -> Result<Vec<String>, UpdateError> {
    let first_of_many = |index: usize| index == 0 && names.len() > 1;
    let mut published = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let name = name.as_ref();
        publish_one(name)?;
        if first_of_many(index) {
            step(BinaryStep::FirstVisible);
        } else if index + 1 == names.len() {
            step(BinaryStep::AllVisible);
        }
        sync_directory(bin_dir)?;
        published.push(name.to_owned());
        if first_of_many(index) {
            step(BinaryStep::First);
        }
    }
    Ok(published)
}

/// Move one staged binary over its installed copy, keeping the old mode.
fn install_staged_binary(staged: &Path, bin_dir: &Path, name: &str) -> Result<(), UpdateError> {
    let from = staged.join(name);
    let to = bin_dir.join(name);
    adopt_permissions(&from, &to)?;
    sync_file(&from, "staged binary")?;
    fs::rename(&from, &to)
        .map_err(|err| UpdateError::Install(format!("could not install {}: {err}", to.display())))
}

/// Commit an install: publish the journal as the rollback backup (keeping
/// any older backup aside until the commit is durable), then drop the older
/// backup best-effort. Returns the backup path.
fn commit_install(
    bin_dir: &Path,
    transaction: &Path,
    checkpoint: &mut impl FnMut(InstallCheckpoint),
) -> Result<PathBuf, UpdateError> {
    let backup = bin_dir.join(BACKUP_DIR);
    let previous_backup = bin_dir.join(PREVIOUS_BACKUP_DIR);
    if backup.exists() {
        fs::rename(&backup, &previous_backup).map_err(|err| {
            UpdateError::Install(format!(
                "could not preserve previous backup {}: {err}",
                backup.display()
            ))
        })?;
        checkpoint(InstallCheckpoint::PreviousBackupVisible);
        sync_directory(bin_dir)?;
        checkpoint(InstallCheckpoint::PreviousBackup);
    }
    fs::rename(transaction, &backup).map_err(|err| {
        UpdateError::Install(format!(
            "could not publish rollback backup {}: {err}",
            backup.display()
        ))
    })?;
    checkpoint(InstallCheckpoint::CommitVisible);
    sync_directory(bin_dir)?;
    checkpoint(InstallCheckpoint::Commit);

    if previous_backup.exists() {
        let _ = fs::remove_dir_all(&previous_backup);
        let _ = sync_directory(bin_dir);
    }
    Ok(backup)
}

/// `fsync` one file; `what` names it in the error.
fn sync_file(path: &Path, what: &str) -> Result<(), UpdateError> {
    fs::File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|err| {
            UpdateError::Install(format!(
                "could not make {what} {} durable: {err}",
                path.display()
            ))
        })
}

/// Remove a directory tree; the error reads "could not `what` `path`".
fn remove_dir(path: &Path, what: &str) -> Result<(), UpdateError> {
    fs::remove_dir_all(path)
        .map_err(|err| UpdateError::Install(format!("could not {what} {}: {err}", path.display())))
}

/// Give the staged file the mode of the file it replaces. With nothing to
/// replace, the mode defaults to `0o755`.
fn adopt_permissions(staged: &Path, target: &Path) -> Result<(), UpdateError> {
    let mode = fs::metadata(target).map_or(0o755, |meta| meta.permissions().mode());
    fs::set_permissions(staged, fs::Permissions::from_mode(mode)).map_err(|err| {
        UpdateError::Install(format!(
            "could not set mode {:o} on {}: {err}",
            mode & 0o7777,
            staged.display()
        ))
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests;
