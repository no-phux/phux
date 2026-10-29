//! The store lock and atomic replacement of the credential file.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use super::{AuthError, CommitOutcome, CredentialFile};

struct StoreLock {
    file: fs::File,
    parent: fs::File,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.file, rustix::fs::FlockOperation::Unlock);
        let _ = rustix::fs::flock(&self.parent, rustix::fs::FlockOperation::Unlock);
    }
}

fn credential_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub(super) fn credential_lock_path(path: &Path) -> PathBuf {
    let parent = credential_parent(path);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("credentials");
    parent.join(format!(".{name}.lock"))
}

fn validate_lock_parent(path: &Path, metadata: &fs::Metadata) -> Result<(), AuthError> {
    if !metadata.file_type().is_dir() {
        return Err(AuthError::InsecureStore(format!(
            "credential parent {} is not a directory",
            path.display()
        )));
    }
    let expected_uid = rustix::process::geteuid().as_raw();
    if metadata.uid() != expected_uid {
        return Err(AuthError::InsecureStore(format!(
            "credential parent {} is owned by uid {}, expected effective uid {expected_uid}",
            path.display(),
            metadata.uid()
        )));
    }
    let mode = metadata.mode() & 0o777;
    if mode & 0o022 != 0 || mode & 0o700 != 0o700 {
        return Err(AuthError::InsecureStore(format!(
            "credential parent {} has mode {mode:04o}; owner rwx and no group/world write permission are required",
            path.display()
        )));
    }
    Ok(())
}

fn validate_lock_file(path: &Path, metadata: &fs::Metadata) -> Result<(), AuthError> {
    if !metadata.file_type().is_file() {
        return Err(AuthError::InsecureStore(format!(
            "credential lock {} is not a regular file",
            path.display()
        )));
    }
    let expected_uid = rustix::process::geteuid().as_raw();
    if metadata.uid() != expected_uid {
        return Err(AuthError::InsecureStore(format!(
            "credential lock {} is owned by uid {}, expected effective uid {expected_uid}",
            path.display(),
            metadata.uid()
        )));
    }
    let mode = metadata.mode() & 0o777;
    if mode != 0o600 {
        return Err(AuthError::InsecureStore(format!(
            "credential lock {} has mode {mode:04o}; expected 0600",
            path.display()
        )));
    }
    Ok(())
}

fn validate_open_path_identity(
    path: &Path,
    open_metadata: &fs::Metadata,
    validate: impl FnOnce(&Path, &fs::Metadata) -> Result<(), AuthError>,
) -> Result<(), AuthError> {
    let path_metadata = fs::symlink_metadata(path)?;
    validate(path, &path_metadata)?;
    if path_metadata.dev() != open_metadata.dev() || path_metadata.ino() != open_metadata.ino() {
        return Err(AuthError::InsecureStore(format!(
            "{} was replaced during lock acquisition",
            path.display()
        )));
    }
    Ok(())
}

fn open_lock_parent(parent: &Path) -> Result<fs::File, AuthError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(parent)?;
    let path_metadata = fs::symlink_metadata(parent)?;
    if path_metadata.file_type().is_symlink() {
        return Err(AuthError::InsecureStore(format!(
            "credential parent {} is a symbolic link",
            parent.display()
        )));
    }
    validate_lock_parent(parent, &path_metadata)?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
        .open(parent)?;
    let open_metadata = directory.metadata()?;
    validate_lock_parent(parent, &open_metadata)?;
    validate_open_path_identity(parent, &open_metadata, validate_lock_parent)?;
    Ok(directory)
}

pub(super) fn with_store_lock<T>(
    path: &Path,
    operation: impl FnOnce() -> Result<T, AuthError>,
) -> Result<T, AuthError> {
    // Every mutation of a store (mint, rotate, revoke, prune, last-seen)
    // takes this lock first, so this is the store's one guard.
    phux_config::production::refuse_dev_on_production_state(path)
        .map_err(AuthError::ProductionStore)?;
    let parent = credential_parent(path);
    let parent_lock = open_lock_parent(parent)?;
    rustix::fs::flock(&parent_lock, rustix::fs::FlockOperation::LockExclusive)
        .map_err(io::Error::from)?;
    let parent_metadata = parent_lock.metadata()?;
    validate_open_path_identity(parent, &parent_metadata, validate_lock_parent)?;

    let lock_path = credential_lock_path(path);
    match fs::symlink_metadata(&lock_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(AuthError::InsecureStore(format!(
                    "credential lock {} is a symbolic link",
                    lock_path.display()
                )));
            }
            validate_lock_file(&lock_path, &metadata)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&lock_path)?;
    validate_lock_file(&lock_path, &lock.metadata()?)?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).map_err(io::Error::from)?;
    let lock_metadata = lock.metadata()?;
    validate_open_path_identity(&lock_path, &lock_metadata, validate_lock_file)?;
    let _guard = StoreLock {
        file: lock,
        parent: parent_lock,
    };
    operation()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AtomicWriteFault {
    None,
    BeforeTempSync,
    AfterTempSyncBeforeRename,
    AfterRenameBeforeDirectorySync,
}

fn injected_failure(stage: &str) -> AuthError {
    io::Error::other(format!("injected atomic-write interruption at {stage}")).into()
}

pub(super) fn atomic_write(path: &Path, file: &CredentialFile) -> Result<CommitOutcome, AuthError> {
    atomic_write_with_fault(path, file, AtomicWriteFault::None)
}

pub(super) fn atomic_write_with_fault(
    path: &Path,
    file: &CredentialFile,
    fault: AtomicWriteFault,
) -> Result<CommitOutcome, AuthError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix)?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("credentials"),
        hex::encode(suffix)
    ));
    let result = (|| {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        serde_json::to_writer_pretty(&mut output, file)
            .map_err(|error| AuthError::Malformed(error.to_string()))?;
        output.write_all(b"\n")?;
        if fault == AtomicWriteFault::BeforeTempSync {
            return Err(injected_failure("before temp-file fsync"));
        }
        output.sync_all()?;
        if fault == AtomicWriteFault::AfterTempSyncBeforeRename {
            return Err(injected_failure("after temp-file fsync"));
        }
        fs::rename(&tmp, path)?;
        if fault == AtomicWriteFault::AfterRenameBeforeDirectorySync {
            return Ok(CommitOutcome { durable: false });
        }
        match FileSync::sync_directory(parent) {
            Ok(()) => Ok(CommitOutcome { durable: true }),
            Err(error) => {
                // Committed but not durable: report that rather than invite a
                // retry that would mint again.
                tracing::warn!(path = %path.display(), %error, "credential store replacement is visible but directory fsync failed; do not retry the mutation");
                Ok(CommitOutcome { durable: false })
            }
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

struct FileSync;

impl FileSync {
    fn sync_directory(path: &Path) -> io::Result<()> {
        fs::File::open(path)?.sync_all()
    }
}
