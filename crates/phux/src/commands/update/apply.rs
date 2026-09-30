//! Verify, unpack, and replace — the half of `phux update` that touches the
//! filesystem.
//!
//! 1. The checksum gates everything: nothing is unpacked or replaced unless the
//!    sidecar digest matches the file on disk.
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
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use super::UpdateError;

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
fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Read the expected digest out of a `.sha256` sidecar (`"<64 hex>  <archive>"`,
/// the `sha256sum` format). A sidecar naming a different artifact is refused.
pub(crate) fn expected_digest(sidecar: &str, archive: &str) -> Result<String, UpdateError> {
    let line = sidecar
        .lines()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| UpdateError::Checksum("the .sha256 sidecar was empty".to_owned()))?;
    let mut fields = line.split_whitespace();
    let digest = fields
        .next()
        .ok_or_else(|| UpdateError::Checksum("the .sha256 sidecar had no digest".to_owned()))?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(UpdateError::Checksum(format!(
            "the .sha256 sidecar did not start with a 64-character hex digest (got `{digest}`)"
        )));
    }
    // `sha256sum -b` prefixes the name with `*`; `shasum -a 256` does not.
    if let Some(named) = fields.next() {
        let named = named.strip_prefix('*').unwrap_or(named);
        if named != archive {
            return Err(UpdateError::Checksum(format!(
                "the .sha256 sidecar is for `{named}`, not `{archive}`"
            )));
        }
    }
    Ok(digest.to_ascii_lowercase())
}

/// Verify `archive` against `sidecar`, returning the agreed digest. This is
/// the trust anchor: everything downstream runs only on `Ok`.
pub(crate) fn verify_archive(
    archive: &Path,
    sidecar: &str,
    archive_name: &str,
) -> Result<String, UpdateError> {
    let expected = expected_digest(sidecar, archive_name)?;
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
    /// The version the saved binaries are.
    pub(crate) previous_version: String,
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
            previous_version: previous_version.to_owned(),
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

/// Save the current binaries into a durable pre-commit journal.
fn prepare_transaction(
    bin_dir: &Path,
    targets: &[&str],
    previous_version: &str,
) -> Result<PathBuf, UpdateError> {
    for name in targets {
        let live = bin_dir.join(name);
        if !live.is_file() {
            return Err(UpdateError::Install(format!(
                "cannot start an update transaction because {} is missing or not a regular file",
                live.display()
            )));
        }
    }

    let transaction = bin_dir.join(TRANSACTION_DIR);
    let preparing = bin_dir.join(PREPARING_DIR);
    fs::create_dir(&preparing).map_err(|err| {
        UpdateError::Install(format!("could not create {}: {err}", preparing.display()))
    })?;

    let mut saved = Vec::new();
    for name in targets {
        let live = bin_dir.join(name);
        let into = preparing.join(name);
        // A hard link keeps the exact inode and mode; copy where links fail.
        if fs::hard_link(&live, &into).is_err() {
            fs::copy(&live, &into).map_err(|err| {
                UpdateError::Install(format!(
                    "could not save {} before replacing it: {err}",
                    live.display()
                ))
            })?;
        }
        fs::File::open(&into)
            .and_then(|file| file.sync_all())
            .map_err(|err| {
                UpdateError::Install(format!(
                    "could not make saved binary {} durable: {err}",
                    into.display()
                ))
            })?;
        saved.push((*name).to_owned());
    }

    let manifest = serde_json::json!({
        "schema_version": 1,
        "version": previous_version,
        "binaries": saved,
        "saved_at": chrono::Utc::now().to_rfc3339(),
    });
    let rendered = serde_json::to_string_pretty(&manifest).map_err(|err| {
        UpdateError::Install(format!("could not render the backup manifest: {err}"))
    })?;
    let manifest_path = preparing.join(BACKUP_MANIFEST);
    let mut manifest_file = fs::File::create(&manifest_path).map_err(|err| {
        UpdateError::Install(format!(
            "could not write {}: {err}",
            manifest_path.display()
        ))
    })?;
    manifest_file
        .write_all(rendered.as_bytes())
        .and_then(|()| manifest_file.sync_all())
        .map_err(|err| {
            UpdateError::Install(format!(
                "could not make {} durable: {err}",
                manifest_path.display()
            ))
        })?;
    sync_directory(&preparing)?;
    fs::rename(&preparing, &transaction).map_err(|err| {
        UpdateError::Install(format!(
            "could not publish transaction journal {}: {err}",
            transaction.display()
        ))
    })?;
    sync_directory(bin_dir)?;
    Ok(transaction)
}

fn sync_directory(path: &Path) -> Result<(), UpdateError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|err| {
            UpdateError::Install(format!(
                "could not make directory {} durable: {err}",
                path.display()
            ))
        })
}

fn manifest_names(directory: &Path) -> Result<Vec<String>, UpdateError> {
    let path = directory.join(BACKUP_MANIFEST);
    let raw = fs::read_to_string(&path).map_err(|err| {
        UpdateError::Install(format!(
            "could not read transaction {}: {err}",
            path.display()
        ))
    })?;
    let manifest: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
        UpdateError::Install(format!(
            "transaction {} is malformed: {err}",
            path.display()
        ))
    })?;
    manifest_binaries(&manifest).ok_or_else(|| {
        UpdateError::Install(format!(
            "transaction {} names no valid release binaries",
            path.display()
        ))
    })
}

/// A manifest's `binaries` list, when it is non-empty and names only release
/// binaries.
fn manifest_binaries(manifest: &serde_json::Value) -> Option<Vec<String>> {
    let names: Vec<String> = manifest
        .get("binaries")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.as_str().map(str::to_owned))
        .collect();
    let valid = !names.is_empty()
        && names
            .iter()
            .all(|name| RELEASE_BINARIES.contains(&name.as_str()));
    valid.then_some(names)
}

/// Publish `source` over `target` without consuming the recovery source.
fn restore_file(source: &Path, target: &Path, bin_dir: &Path) -> Result<(), UpdateError> {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("binary");
    let temporary = bin_dir.join(format!(".phux-update-recover-{name}"));
    let _ = fs::remove_file(&temporary);
    if fs::hard_link(source, &temporary).is_err() {
        fs::copy(source, &temporary).map_err(|err| {
            UpdateError::Install(format!(
                "could not stage recovery of {}: {err}",
                target.display()
            ))
        })?;
    }
    sync_file(&temporary, "recovered binary")?;
    fs::rename(&temporary, target).map_err(|err| {
        UpdateError::Install(format!("could not recover {}: {err}", target.display()))
    })?;
    sync_directory(bin_dir)
}

/// Recover any state whose durable commit marker was not published.
///
/// Phases run in order: drop an unpublished journal, finish a committed
/// rollback, roll back an uncommitted install or rollback, then settle a
/// leftover previous backup.
fn recover_interrupted_transaction(bin_dir: &Path) -> Result<(), UpdateError> {
    clear_incomplete_journal(bin_dir)?;
    finish_committed_rollback(bin_dir)?;
    undo_uncommitted_transaction(bin_dir)?;
    settle_previous_backup(bin_dir)
}

/// A journal still being prepared never published anything: drop it.
fn clear_incomplete_journal(bin_dir: &Path) -> Result<(), UpdateError> {
    let preparing = bin_dir.join(PREPARING_DIR);
    if !preparing.exists() {
        return Ok(());
    }
    remove_dir(&preparing, "clear incomplete journal")?;
    sync_directory(bin_dir)
}

/// A rollback whose commit marker is durable only has cleanup left: the
/// consumed backup goes (durably) before the marker does.
fn finish_committed_rollback(bin_dir: &Path) -> Result<(), UpdateError> {
    let committed_rollback = bin_dir.join(ROLLBACK_COMMITTED_DIR);
    if !committed_rollback.exists() {
        return Ok(());
    }
    let backup = bin_dir.join(BACKUP_DIR);
    if backup.exists() {
        remove_dir(&backup, "finish committed rollback at")?;
        sync_directory(bin_dir)?;
    }
    remove_dir(&committed_rollback, "clear committed rollback")?;
    sync_directory(bin_dir)
}

/// An uncommitted transaction journal: restore every journaled binary
/// (all-or-nothing on the journal being complete), put a set-aside backup
/// back, and drop the journal.
fn undo_uncommitted_transaction(bin_dir: &Path) -> Result<(), UpdateError> {
    let transaction = bin_dir.join(TRANSACTION_DIR);
    if !transaction.exists() {
        return Ok(());
    }
    let names = manifest_names(&transaction)?;
    if let Some(saved) = names
        .iter()
        .map(|name| transaction.join(name))
        .find(|saved| !saved.is_file())
    {
        return Err(UpdateError::Install(format!(
            "interrupted transaction is missing {}",
            saved.display()
        )));
    }
    for name in &names {
        restore_file(&transaction.join(name), &bin_dir.join(name), bin_dir)?;
    }

    let previous = bin_dir.join(PREVIOUS_BACKUP_DIR);
    let backup = bin_dir.join(BACKUP_DIR);
    if previous.exists() && !backup.exists() {
        fs::rename(&previous, &backup).map_err(|err| {
            UpdateError::Install(format!(
                "could not restore previous rollback backup {}: {err}",
                backup.display()
            ))
        })?;
        sync_directory(bin_dir)?;
    }
    remove_dir(&transaction, "clear recovered transaction")?;
    sync_directory(bin_dir)
}

/// A previous backup left beside a committed one is superseded; without a
/// committed one it becomes the backup again.
fn settle_previous_backup(bin_dir: &Path) -> Result<(), UpdateError> {
    let previous = bin_dir.join(PREVIOUS_BACKUP_DIR);
    if !previous.exists() {
        return Ok(());
    }
    let backup = bin_dir.join(BACKUP_DIR);
    if backup.exists() {
        remove_dir(&previous, "clear superseded backup")?;
    } else {
        fs::rename(&previous, &backup).map_err(|err| {
            UpdateError::Install(format!(
                "could not recover previous backup {}: {err}",
                backup.display()
            ))
        })?;
    }
    sync_directory(bin_dir)
}

/// What a rollback restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RolledBack {
    /// The binaries put back.
    pub(crate) binaries: Vec<String>,
    /// The version they are.
    pub(crate) version: String,
}

/// Restore the binaries saved by the last successful update. Uses the same
/// durable journal; the backup is consumed only after the commit
/// rename is durable. Nothing saved is an error, not a no-op.
pub(crate) fn rollback(bin_dir: &Path) -> Result<RolledBack, UpdateError> {
    rollback_with_checkpoint(bin_dir, |_| {})
}

fn read_backup_manifest(backup: &Path) -> Result<(String, Vec<String>), UpdateError> {
    let manifest_path = backup.join(BACKUP_MANIFEST);
    let raw = fs::read_to_string(&manifest_path).map_err(|err| {
        UpdateError::NoBackup(format!("no saved binaries at {}: {err}", backup.display()))
    })?;
    let manifest: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
        UpdateError::NoBackup(format!(
            "{} is not readable: {err}",
            manifest_path.display()
        ))
    })?;
    let version = manifest
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let names = manifest_binaries(&manifest).ok_or_else(|| {
        UpdateError::NoBackup(format!(
            "{} lists no valid saved binaries",
            manifest_path.display()
        ))
    })?;
    Ok((version, names))
}

fn rollback_with_checkpoint(
    bin_dir: &Path,
    mut checkpoint: impl FnMut(RollbackCheckpoint),
) -> Result<RolledBack, UpdateError> {
    let _lock = UpdateLock::acquire(bin_dir)?;
    let backup = bin_dir.join(BACKUP_DIR);
    let (version, names) = read_backup_manifest(&backup)?;

    // All-or-nothing: refuse if any saved file is missing.
    if let Some(saved) = names
        .iter()
        .map(|name| backup.join(name))
        .find(|saved| !saved.exists())
    {
        return Err(UpdateError::NoBackup(format!(
            "{} is missing; the backup is incomplete and was not applied",
            saved.display()
        )));
    }

    let target_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let transaction = prepare_transaction(bin_dir, &target_refs, "unknown")?;
    checkpoint(RollbackCheckpoint::Journal);

    let restore = (|| {
        let restored = publish_binaries(
            bin_dir,
            &names,
            |name| restore_saved_binary(bin_dir, &backup, &transaction, name),
            |step| checkpoint(step.into()),
        )?;
        checkpoint(RollbackCheckpoint::AllBinaries);
        commit_rollback(bin_dir, &transaction, &backup, &mut checkpoint)?;
        Ok(RolledBack {
            binaries: restored,
            version,
        })
    })();

    if restore.is_err()
        && transaction.exists()
        && let Err(recovery) = recover_interrupted_transaction(bin_dir)
    {
        return Err(UpdateError::Install(format!(
            "rollback failed and recovery also failed: {recovery}"
        )));
    }
    restore
}

/// Stage one saved binary inside the journal (hard link, else copy), make
/// it durable, and rename it over the installed copy.
fn restore_saved_binary(
    bin_dir: &Path,
    backup: &Path,
    transaction: &Path,
    name: &str,
) -> Result<(), UpdateError> {
    let saved = backup.join(name);
    let target = bin_dir.join(name);
    let incoming = transaction.join(format!("{name}.incoming"));
    if fs::hard_link(&saved, &incoming).is_err() {
        fs::copy(&saved, &incoming).map_err(|err| {
            UpdateError::Install(format!(
                "could not stage rollback of {}: {err}",
                target.display()
            ))
        })?;
    }
    sync_file(&incoming, "rollback binary")?;
    fs::rename(&incoming, &target).map_err(|err| {
        UpdateError::Install(format!(
            "could not restore {} from {}: {err}",
            target.display(),
            saved.display()
        ))
    })
}

/// Commit a rollback: publish the journal as the commit marker, then consume
/// the backup and finally the marker.
fn commit_rollback(
    bin_dir: &Path,
    transaction: &Path,
    backup: &Path,
    checkpoint: &mut impl FnMut(RollbackCheckpoint),
) -> Result<(), UpdateError> {
    let committed = bin_dir.join(ROLLBACK_COMMITTED_DIR);
    fs::rename(transaction, &committed).map_err(|err| {
        UpdateError::Install(format!(
            "could not commit rollback at {}: {err}",
            committed.display()
        ))
    })?;
    checkpoint(RollbackCheckpoint::CommitVisible);
    sync_directory(bin_dir)?;
    checkpoint(RollbackCheckpoint::Commit);

    remove_dir(backup, "clear consumed backup")?;
    // The consumed backup must be durably absent before the commit marker
    // can disappear. Otherwise a crash could resurrect the backup without
    // the marker and make a second rollback apply it in the wrong direction.
    sync_directory(bin_dir)?;
    remove_dir(&committed, "clear committed rollback")?;
    sync_directory(bin_dir)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::panic::AssertUnwindSafe;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{
        BACKUP_DIR, InstallCheckpoint, PREPARING_DIR, RollbackCheckpoint, Staging, TRANSACTION_DIR,
        UpdateError, UpdateLock, expected_digest, replace_binaries,
        replace_binaries_with_checkpoint, rollback, rollback_with_checkpoint, sha256_file,
        unpack_verified, verify_archive,
    };

    /// The SHA-256 of the empty input, the standard published vector.
    #[test]
    fn sha256_matches_the_published_vector() {
        let scratch = tempfile::tempdir().unwrap();
        let empty = scratch.path().join("empty");
        fs::write(&empty, b"").unwrap();
        assert_eq!(
            sha256_file(&empty).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let abc = scratch.path().join("abc");
        fs::write(&abc, b"abc").unwrap();
        assert_eq!(
            sha256_file(&abc).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// The release workflow's sidecar format (with or without `sha256sum -b`'s
    /// marker, or digest-only) parses; malformed or crossed sidecars do not.
    #[test]
    fn sidecar_parsing_accepts_the_release_format_and_refuses_the_rest() {
        let archive = "phux-v1.0.0-x.tar.gz";
        let digest = "a".repeat(64);
        for sidecar in [
            format!("{digest}  {archive}\n"),
            format!("{digest} *{archive}\n"),
            digest.clone(),
        ] {
            assert_eq!(expected_digest(&sidecar, archive).unwrap(), digest);
        }
        for (sidecar, why) in [
            (String::new(), "empty"),
            (format!("not-a-digest  {archive}"), "short"),
            (format!("{}  x.tar.gz", "z".repeat(64)), "non-hex"),
            (
                format!("{digest}  phux-v9.9.9-other.tar.gz"),
                "names another artifact",
            ),
        ] {
            assert!(
                expected_digest(&sidecar, archive).is_err(),
                "sidecar should be refused ({why})"
            );
        }
    }

    /// The gate: a tampered archive is refused, loudly, with both digests.
    #[test]
    fn a_checksum_mismatch_refuses_and_names_both_digests() {
        let scratch = tempfile::tempdir().unwrap();
        let archive = scratch.path().join("phux-v1.0.0-t.tar.gz");
        fs::write(&archive, b"the real bytes").unwrap();
        let good = sha256_file(&archive).unwrap();

        // Matching sidecar: accepted, and the digest is handed back.
        let sidecar = format!("{good}  phux-v1.0.0-t.tar.gz\n");
        assert_eq!(
            verify_archive(&archive, &sidecar, "phux-v1.0.0-t.tar.gz").unwrap(),
            good
        );

        // Someone swapped the archive after the sidecar was published.
        fs::write(&archive, b"tampered bytes").unwrap();
        let err = verify_archive(&archive, &sidecar, "phux-v1.0.0-t.tar.gz").unwrap_err();
        match err {
            UpdateError::ChecksumMismatch {
                expected, actual, ..
            } => {
                assert_eq!(expected, good);
                assert_ne!(actual, good);
            }
            other => panic!("expected a checksum mismatch, got {other:?}"),
        }
    }

    const CURRENT_RELEASE_DOCS: &[&str] =
        &["README.md", "LICENSE", "NOTICE", "THIRD-PARTY-NOTICES.md"];
    const PRE_RELICENSE_DOCS: &[&str] = &["README.md", "LICENSE-MIT", "LICENSE-APACHE"];

    /// Build a release-shaped tarball in `dir`, returning its path.
    fn build_archive(dir: &Path, stage: &str, extra: Option<(&str, &[u8])>) -> PathBuf {
        build_archive_with_docs(dir, stage, CURRENT_RELEASE_DOCS, extra)
    }

    fn build_archive_with_docs(
        dir: &Path,
        stage: &str,
        docs: &[&str],
        extra: Option<(&str, &[u8])>,
    ) -> PathBuf {
        let staging = dir.join("build").join(stage);
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("phux"), b"#!/bin/sh\nnew phux\n").unwrap();
        fs::set_permissions(staging.join("phux"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(staging.join("phux-mcp"), b"#!/bin/sh\nnew mcp\n").unwrap();
        fs::set_permissions(staging.join("phux-mcp"), fs::Permissions::from_mode(0o755)).unwrap();
        for name in docs {
            fs::write(staging.join(name), name.as_bytes()).unwrap();
        }
        if let Some((name, bytes)) = extra {
            fs::write(staging.join(name), bytes).unwrap();
        }
        let archive = dir.join(format!("{stage}.tar.gz"));
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(dir.join("build"))
            .arg(stage)
            .status()
            .unwrap();
        assert!(status.success());
        archive
    }

    #[test]
    fn a_release_shaped_archive_unpacks_and_an_unexpected_member_is_refused() {
        let scratch = tempfile::tempdir().unwrap();
        let stage = "phux-v1.0.0-aarch64-apple-darwin";

        let archive = build_archive(scratch.path(), stage, None);
        let into = scratch.path().join("into");
        fs::create_dir(&into).unwrap();
        let staged = unpack_verified(&archive, stage, &into).unwrap();
        assert!(staged.join("phux").is_file());
        assert!(staged.join("phux-mcp").is_file());

        // Pre-relicense (<= v0.35.0) tarballs carry the old license files.
        let old = tempfile::tempdir().unwrap();
        let old_stage = "phux-v0.35.0-aarch64-apple-darwin";
        let archive = build_archive_with_docs(old.path(), old_stage, PRE_RELICENSE_DOCS, None);
        let staged = unpack_verified(&archive, old_stage, old.path()).unwrap();
        assert!(staged.join("LICENSE-APACHE").is_file());

        let hostile = tempfile::tempdir().unwrap();
        let archive = build_archive(hostile.path(), stage, Some(("payload.sh", b"rm -rf /")));
        let into = hostile.path().join("into");
        fs::create_dir(&into).unwrap();
        let err = unpack_verified(&archive, stage, &into).unwrap_err();
        assert!(
            format!("{err}").contains("payload.sh"),
            "the refusal must name the unexpected member: {err}"
        );
        assert!(
            !into.join(stage).exists(),
            "nothing may be extracted when the listing is refused"
        );
    }

    /// Seed a bin directory with a "current" install at a chosen mode.
    fn seed_bin_dir(dir: &Path, mode: u32) {
        fs::write(dir.join("phux"), b"#!/bin/sh\nold phux\n").unwrap();
        fs::set_permissions(dir.join("phux"), fs::Permissions::from_mode(mode)).unwrap();
        fs::write(dir.join("phux-mcp"), b"#!/bin/sh\nold mcp\n").unwrap();
        fs::set_permissions(dir.join("phux-mcp"), fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn replacement_is_atomic_preserves_permissions_and_is_reversible() {
        let scratch = tempfile::tempdir().unwrap();
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin).unwrap();
        seed_bin_dir(&bin, 0o700);

        let stage = "phux-v1.0.0-aarch64-apple-darwin";
        let archive = build_archive(scratch.path(), stage, None);
        let staging = Staging::create(&bin).unwrap();
        let staged = unpack_verified(&archive, stage, staging.path()).unwrap();

        let replaced = replace_binaries(&bin, &staged, "0.12.1").unwrap();
        assert_eq!(replaced.binaries, vec!["phux-mcp", "phux"]);
        assert_eq!(replaced.previous_version, "0.12.1");
        assert_eq!(replaced.backup, bin.join(BACKUP_DIR));

        assert_eq!(
            fs::read(bin.join("phux")).unwrap(),
            b"#!/bin/sh\nnew phux\n"
        );
        assert_eq!(
            fs::read(bin.join("phux-mcp")).unwrap(),
            b"#!/bin/sh\nnew mcp\n"
        );
        for name in ["phux", "phux-mcp"] {
            let mode = fs::metadata(bin.join(name)).unwrap().permissions().mode() & 0o7777;
            assert_eq!(mode, 0o700, "{name} kept the wrong mode");
        }
        assert!(!bin.join("phux.new").exists());

        let back = rollback(&bin).unwrap();
        assert_eq!(back.version, "0.12.1");
        assert_eq!(
            fs::read(bin.join("phux")).unwrap(),
            b"#!/bin/sh\nold phux\n"
        );
        assert_eq!(
            fs::read(bin.join("phux-mcp")).unwrap(),
            b"#!/bin/sh\nold mcp\n"
        );
        assert!(!bin.join(BACKUP_DIR).exists());
    }

    fn staged_release(scratch: &tempfile::TempDir, bin: &Path) -> (Staging, PathBuf) {
        let stage = "phux-v1.0.0-aarch64-apple-darwin";
        let archive = build_archive(scratch.path(), stage, None);
        let staging = Staging::create(bin).unwrap();
        let staged = unpack_verified(&archive, stage, staging.path()).unwrap();
        (staging, staged)
    }

    fn assert_pair(bin: &Path, phux: &[u8], mcp: &[u8]) {
        assert_eq!(fs::read(bin.join("phux")).unwrap(), phux);
        assert_eq!(fs::read(bin.join("phux-mcp")).unwrap(), mcp);
    }

    #[test]
    fn interrupted_install_recovers_old_pair_until_the_durable_commit() {
        let checkpoints = [
            InstallCheckpoint::Journal,
            InstallCheckpoint::FirstBinaryVisible,
            InstallCheckpoint::FirstBinary,
            InstallCheckpoint::AllBinariesVisible,
            InstallCheckpoint::AllBinaries,
            InstallCheckpoint::PreviousBackupVisible,
            InstallCheckpoint::PreviousBackup,
            InstallCheckpoint::CommitVisible,
            InstallCheckpoint::Commit,
        ];

        for fault in checkpoints {
            let scratch = tempfile::tempdir().unwrap();
            let bin = scratch.path().join("bin");
            fs::create_dir(&bin).unwrap();
            seed_bin_dir(&bin, 0o755);

            // The older backup generation must survive until the new commit.
            if matches!(
                fault,
                InstallCheckpoint::PreviousBackupVisible
                    | InstallCheckpoint::PreviousBackup
                    | InstallCheckpoint::CommitVisible
                    | InstallCheckpoint::Commit
            ) {
                let previous = bin.join(BACKUP_DIR);
                fs::create_dir(&previous).unwrap();
                fs::write(previous.join("sentinel"), b"older backup").unwrap();
            }

            let (_staging, staged) = staged_release(&scratch, &bin);
            let interrupted = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let _ = replace_binaries_with_checkpoint(&bin, &staged, "0.12.1", |at| {
                    assert_ne!(at, fault, "injected interruption at {at:?}");
                });
            }));
            assert!(interrupted.is_err(), "checkpoint {fault:?} was not reached");

            // The next lock holder repairs the journal before any new work.
            drop(UpdateLock::acquire(&bin).unwrap());
            if matches!(
                fault,
                InstallCheckpoint::CommitVisible | InstallCheckpoint::Commit
            ) {
                assert_pair(&bin, b"#!/bin/sh\nnew phux\n", b"#!/bin/sh\nnew mcp\n");
            } else {
                assert_pair(&bin, b"#!/bin/sh\nold phux\n", b"#!/bin/sh\nold mcp\n");
            }
            assert!(
                !bin.join(TRANSACTION_DIR).exists(),
                "journal survived recovery at {fault:?}"
            );
        }
    }

    #[test]
    fn interrupted_rollback_recovers_new_pair_until_the_durable_commit() {
        let checkpoints = [
            RollbackCheckpoint::Journal,
            RollbackCheckpoint::FirstBinaryVisible,
            RollbackCheckpoint::FirstBinary,
            RollbackCheckpoint::AllBinariesVisible,
            RollbackCheckpoint::AllBinaries,
            RollbackCheckpoint::CommitVisible,
            RollbackCheckpoint::Commit,
        ];

        for fault in checkpoints {
            let scratch = tempfile::tempdir().unwrap();
            let bin = scratch.path().join("bin");
            fs::create_dir(&bin).unwrap();
            seed_bin_dir(&bin, 0o755);
            let (_staging, staged) = staged_release(&scratch, &bin);
            replace_binaries(&bin, &staged, "0.12.1").unwrap();

            let interrupted = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let _ = rollback_with_checkpoint(&bin, |at| {
                    assert_ne!(at, fault, "injected interruption at {at:?}");
                });
            }));
            assert!(interrupted.is_err(), "checkpoint {fault:?} was not reached");

            drop(UpdateLock::acquire(&bin).unwrap());
            if matches!(
                fault,
                RollbackCheckpoint::CommitVisible | RollbackCheckpoint::Commit
            ) {
                assert_pair(&bin, b"#!/bin/sh\nold phux\n", b"#!/bin/sh\nold mcp\n");
                assert!(!bin.join(BACKUP_DIR).exists());
            } else {
                assert_pair(&bin, b"#!/bin/sh\nnew phux\n", b"#!/bin/sh\nnew mcp\n");
                assert!(bin.join(BACKUP_DIR).exists());
            }
            assert!(!bin.join(TRANSACTION_DIR).exists());
        }
    }

    #[test]
    fn update_lock_serializes_publishers() {
        let scratch = tempfile::tempdir().unwrap();
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let first = UpdateLock::acquire(&bin).unwrap();
        let second_bin = bin;
        let (sent, received) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _second = UpdateLock::acquire(&second_bin).unwrap();
            sent.send(()).unwrap();
        });

        assert!(
            matches!(
                received.recv_timeout(Duration::from_millis(100)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "a second publisher acquired the lock concurrently"
        );
        drop(first);
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn an_interrupted_journal_build_is_discarded_before_publication() {
        let scratch = tempfile::tempdir().unwrap();
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin).unwrap();
        seed_bin_dir(&bin, 0o755);
        let preparing = bin.join(PREPARING_DIR);
        fs::create_dir(&preparing).unwrap();
        fs::write(preparing.join("phux"), b"incomplete journal").unwrap();

        drop(UpdateLock::acquire(&bin).unwrap());

        assert!(!preparing.exists());
        assert_pair(&bin, b"#!/bin/sh\nold phux\n", b"#!/bin/sh\nold mcp\n");
    }

    #[test]
    fn a_lone_phux_install_does_not_grow_a_phux_mcp() {
        let scratch = tempfile::tempdir().unwrap();
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin).unwrap();
        fs::write(bin.join("phux"), b"old").unwrap();
        fs::set_permissions(bin.join("phux"), fs::Permissions::from_mode(0o755)).unwrap();

        let stage = "phux-v1.0.0-aarch64-apple-darwin";
        let archive = build_archive(scratch.path(), stage, None);
        let staging = Staging::create(&bin).unwrap();
        let staged = unpack_verified(&archive, stage, staging.path()).unwrap();

        let replaced = replace_binaries(&bin, &staged, "0.12.1").unwrap();
        assert_eq!(replaced.binaries, vec!["phux"]);
        assert!(
            !bin.join("phux-mcp").exists(),
            "an install without phux-mcp must not acquire one"
        );
    }

    #[test]
    fn rollback_without_a_backup_is_an_error_not_a_no_op() {
        let scratch = tempfile::tempdir().unwrap();
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin).unwrap();
        seed_bin_dir(&bin, 0o755);
        let err = rollback(&bin).unwrap_err();
        assert!(matches!(err, UpdateError::NoBackup(_)), "{err:?}");
        assert_eq!(
            fs::read(bin.join("phux")).unwrap(),
            b"#!/bin/sh\nold phux\n"
        );
    }

    #[test]
    fn staging_is_a_sibling_of_the_target_and_is_cleaned_up() {
        let scratch = tempfile::tempdir().unwrap();
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let path = {
            let staging = Staging::create(&bin).unwrap();
            assert_eq!(staging.path().parent(), Some(bin.as_path()));
            staging.path().to_path_buf()
        };
        assert!(!path.exists(), "the staging directory must be removed");
    }

    #[test]
    fn staging_refuses_an_unwritable_bin_directory() {
        let scratch = tempfile::tempdir().unwrap();
        let bin = scratch.path().join("bin");
        fs::create_dir(&bin).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o500)).unwrap();
        let result = Staging::create(&bin);
        // Restore the mode so the scratch guard can clean up.
        let _ = fs::set_permissions(&bin, fs::Permissions::from_mode(0o700));
        let Err(err) = result else {
            // Running as root: mode bits do not stop the write.
            return;
        };
        assert!(
            format!("{err}").contains("must be writable"),
            "the refusal must say what is wrong: {err}"
        );
    }
}
