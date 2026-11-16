//! Secure-file read, lock, and replace checks.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::*;

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
    assert!(matches!(error, SecureFileError::Insecure { .. }), "{error}");
    assert!(!error.to_string().contains("secret-location"), "{error}");
    // The public certificate tolerates world read, never world write.
    assert!(read_stable(&file, FileRole::CaCertificate).is_ok());

    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(matches!(
        read_stable(&link, FileRole::CaCertificate),
        Err(SecureFileError::Insecure { .. })
    ));
    assert!(matches!(
        read_stable(dir.path(), FileRole::Registry),
        Err(SecureFileError::Insecure { .. })
    ));
}

#[test]
fn replacement_is_owner_only_and_leaves_no_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workload-keys");
    owner_only(&path, b"old");
    let outcome = with_lock(&path, |held| atomic_replace(&path, b"new", held)).unwrap();
    assert!(outcome.is_durable());
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
        with_lock(&path, |_| Ok::<_, SecureFileError>(())),
        Err(SecureFileError::Insecure { .. })
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
        Err(SecureFileError::Insecure {
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
        ".remote-tokens.00112233445566ff.tmp",
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

/// The bearer store is owner-only: group or world bits, an owner who
/// cannot read it, or another owner are each refused.
#[test]
fn the_bearer_store_is_owner_only_and_owner_readable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("remote-tokens");
    owner_only(&path, b"{}");
    assert!(read_stable(&path, FileRole::BearerStore).is_ok());
    for mode in [0o640, 0o604, 0o200] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(
            matches!(
                read_stable(&path, FileRole::BearerStore),
                Err(SecureFileError::Insecure { .. })
            ),
            "mode {mode:o}"
        );
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let meta = std::fs::symlink_metadata(&path).unwrap();
    assert!(validate_owner_file(&meta, FileRole::BearerStore).is_ok());
    assert!(matches!(
        validate_owner_file_as(
            &meta,
            FileRole::BearerStore,
            effective_uid().wrapping_add(1)
        ),
        Err(SecureFileError::Insecure { .. })
    ));
}

/// Each store keeps its own error split: integrity failures stay insecure,
/// I/O and unstable reads stay unreadable (`StoreCondition::of`).
#[test]
fn errors_map_into_each_store_by_kind() {
    use crate::auth::AuthError;
    use crate::workload::WorkloadError;

    let insecure = || insecure("credential store", "is a symbolic link");
    assert!(matches!(
        AuthError::from(insecure()),
        AuthError::InsecureStore(reason) if reason == "credential store is a symbolic link"
    ));
    assert!(matches!(
        AuthError::from(SecureFileError::ProductionState("no".to_owned())),
        AuthError::ProductionStore(_)
    ));
    assert!(matches!(
        AuthError::from(SecureFileError::Unstable),
        AuthError::UnstableStore
    ));
    assert!(matches!(
        AuthError::from(SecureFileError::Io(std::io::Error::other("x"))),
        AuthError::Io(_)
    ));
    assert!(matches!(
        WorkloadError::from(insecure()),
        WorkloadError::Insecure {
            file: "credential store",
            reason: "is a symbolic link"
        }
    ));
    assert!(matches!(
        WorkloadError::from(SecureFileError::ProductionState("no".to_owned())),
        WorkloadError::ProductionState(_)
    ));
    assert!(matches!(
        WorkloadError::from(SecureFileError::Unstable),
        WorkloadError::Unstable
    ));
}

/// An interrupted replacement leaves the old file and no temporary; one
/// interrupted after the rename is visible and reported as not durable.
#[test]
fn an_interrupted_replacement_is_old_or_new() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("remote-tokens");
    owner_only(&path, b"old");
    for stage in [
        WriteFault::BeforeTempSync,
        WriteFault::AfterTempSyncBeforeRename,
    ] {
        let _armed = fault::inject(stage);
        assert!(with_lock(&path, |held| atomic_replace(&path, b"new", held)).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"old", "{stage:?}");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "{stage:?}"
        );
    }
    let _armed = fault::inject(WriteFault::AfterRenameBeforeDirectorySync);
    let outcome = with_lock(&path, |held| atomic_replace(&path, b"new", held)).unwrap();
    assert!(!outcome.is_durable());
    assert_eq!(std::fs::read(&path).unwrap(), b"new");
}
