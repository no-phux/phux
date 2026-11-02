use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use super::transaction::rollback_with_checkpoint;
use super::{
    BACKUP_DIR, InstallCheckpoint, PREPARING_DIR, RollbackCheckpoint, Staging, TRANSACTION_DIR,
    UpdateError, UpdateLock, replace_binaries, replace_binaries_with_checkpoint, rollback,
    sha256_file, unpack_verified, verify_digest,
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

/// The gate: a tampered archive is refused, loudly, with both digests.
#[test]
fn a_checksum_mismatch_refuses_and_names_both_digests() {
    let scratch = tempfile::tempdir().unwrap();
    let archive = scratch.path().join("phux-v1.0.0-t.tar.gz");
    fs::write(&archive, b"the real bytes").unwrap();
    let good = sha256_file(&archive).unwrap();

    assert_eq!(
        verify_digest(&archive, &good, "phux-v1.0.0-t.tar.gz").unwrap(),
        good
    );

    // Someone swapped the archive after the digest was published.
    fs::write(&archive, b"tampered bytes").unwrap();
    let err = verify_digest(&archive, &good, "phux-v1.0.0-t.tar.gz").unwrap_err();
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

const CURRENT_RELEASE_DOCS: &[&str] = &["README.md", "LICENSE", "NOTICE", "THIRD-PARTY-NOTICES.md"];
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

/// Recovery and rollback leave the binaries alone when the manifest is from a
/// newer format, since they cannot know what it lists.
#[test]
fn a_newer_manifest_stops_recovery_and_rollback() {
    let scratch = tempfile::tempdir().unwrap();
    let bin = scratch.path().join("bin");
    fs::create_dir(&bin).unwrap();
    seed_bin_dir(&bin, 0o755);
    let (_staging, staged) = staged_release(&scratch, &bin);
    replace_binaries(&bin, &staged, "0.12.1").unwrap();
    let new_pair = (b"#!/bin/sh\nnew phux\n", b"#!/bin/sh\nnew mcp\n");

    let backup = bin.join(BACKUP_DIR);
    let manifest_path = backup.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["schema_version"] = serde_json::json!(super::transaction::MANIFEST_SCHEMA + 1);
    fs::write(&manifest_path, manifest.to_string()).unwrap();
    let err = rollback(&bin).unwrap_err();
    assert!(err.to_string().contains("newer phux"), "{err}");
    assert_pair(&bin, new_pair.0, new_pair.1);

    let journal = bin.join(TRANSACTION_DIR);
    fs::create_dir(&journal).unwrap();
    fs::write(journal.join("manifest.json"), manifest.to_string()).unwrap();
    let Err(err) = UpdateLock::acquire(&bin) else {
        panic!("recovery ran on a newer journal");
    };
    assert!(err.to_string().contains("newer phux"), "{err}");
    assert_pair(&bin, new_pair.0, new_pair.1);
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
