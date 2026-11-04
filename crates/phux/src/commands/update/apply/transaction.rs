//! The durable update transaction: the journal an install or rollback writes
//! first, the crash recovery that reads it, and rollback itself.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{
    BACKUP_DIR, BACKUP_MANIFEST, PREPARING_DIR, PREVIOUS_BACKUP_DIR, RELEASE_BINARIES,
    ROLLBACK_COMMITTED_DIR, RollbackCheckpoint, TRANSACTION_DIR, UpdateError, UpdateLock,
    publish_binaries, remove_dir, sync_file,
};

/// Schema of the backup manifest this binary writes and the newest it reads.
pub(super) const MANIFEST_SCHEMA: u64 = 1;

/// Save the current binaries into a durable pre-commit journal.
pub(super) fn prepare_transaction(
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
        "schema_version": MANIFEST_SCHEMA,
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

pub(super) fn sync_directory(path: &Path) -> Result<(), UpdateError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|err| {
            UpdateError::Install(format!(
                "could not make directory {} durable: {err}",
                path.display()
            ))
        })
}

/// Whether this binary reads `manifest`'s format. Recovery and rollback rename
/// live binaries from what it lists, so a newer format stops them with the
/// install as it stands.
fn manifest_schema_supported(manifest: &serde_json::Value) -> bool {
    manifest.get("schema_version").is_none_or(|schema| {
        schema
            .as_u64()
            .is_some_and(|schema| schema <= MANIFEST_SCHEMA)
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
    if !manifest_schema_supported(&manifest) {
        return Err(UpdateError::Install(format!(
            "transaction {} is from a newer phux; finish it with that release",
            path.display()
        )));
    }
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
pub(super) fn recover_interrupted_transaction(bin_dir: &Path) -> Result<(), UpdateError> {
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
    if !manifest_schema_supported(&manifest) {
        return Err(UpdateError::NoBackup(format!(
            "{} is from a newer phux; roll back with that release",
            manifest_path.display()
        )));
    }
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

pub(super) fn rollback_with_checkpoint(
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
