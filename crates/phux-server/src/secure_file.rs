//! Owner-only, no-follow, lock-and-rename persistence for the files that
//! decide who connects: the workload authority files and the bearer
//! credential store (`docs/remote.md#who-can-connect`).
//!
//! Reads check owner control of the file and its directory, re-validate
//! across a no-follow open, and accept only a stat-stable read. Writes hold
//! an exclusive `flock` on the directory and commit via temp file, fsync,
//! rename, and directory fsync, sweeping stale temporaries only in the
//! locked directory. Ancestors must also be owner (or root) controlled.
//! File buffers are sized from metadata and scrubbed when not returned
//! (best effort). Diagnostics name a file's role, never its path.
#![allow(
    clippy::redundant_pub_crate,
    reason = "crate-private substrate: the auth and workload stores reach these through this module"
)]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

mod error;
mod read;
mod write;

pub(crate) use self::error::SecureFileError;
pub(crate) use self::read::{probe, read_stable};
pub use self::write::CommitOutcome;
pub(crate) use self::write::{HeldLock, atomic_replace, open_owner_dir, with_lock};
#[cfg(test)]
pub(crate) use self::write::{WriteFault, fault};

/// The role a file plays: the only name a diagnostic gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileRole {
    /// `<state-dir>/workload-keys`.
    Registry,
    /// `<state-dir>/workload-ca.pem` (public).
    CaCertificate,
    /// `<state-dir>/workload-ca.key` (secret).
    CaPrivateKey,
    /// `<state-dir>/enrollment-tickets` (hashes only, still owner-only).
    Tickets,
    /// `<state-dir>/workload-org-roots/<org>.pem` (public, still owner-only:
    /// whoever can change it decides who connects).
    OrgRoots,
    /// `<state-dir>/remote-tokens` (bearer verifiers, owner-only).
    BearerStore,
}

impl FileRole {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Registry => "registry",
            Self::CaCertificate => "CA certificate",
            Self::CaPrivateKey => "CA private key",
            Self::Tickets => "enrollment tickets",
            Self::OrgRoots => "organization roots",
            Self::BearerStore => "credential store",
        }
    }

    /// Mode bits this role may not carry (the public CA cert only refuses
    /// group/world write).
    const fn forbidden_mode_bits(self) -> u32 {
        match self {
            Self::CaCertificate => 0o022,
            Self::Registry
            | Self::CaPrivateKey
            | Self::Tickets
            | Self::OrgRoots
            | Self::BearerStore => 0o077,
        }
    }
}

const fn insecure(file: &'static str, reason: &'static str) -> SecureFileError {
    SecureFileError::Insecure { file, reason }
}

/// The identity and content generation of one file, from `lstat`/`fstat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
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

fn validate_owner_file(meta: &fs::Metadata, role: FileRole) -> Result<(), SecureFileError> {
    validate_owner_file_as(meta, role, effective_uid())
}

fn validate_owner_file_as(
    meta: &fs::Metadata,
    role: FileRole,
    owner: u32,
) -> Result<(), SecureFileError> {
    let file = role.as_str();
    if meta.file_type().is_symlink() {
        return Err(insecure(file, "is a symbolic link"));
    }
    if !meta.file_type().is_file() {
        return Err(insecure(file, "is not a regular file"));
    }
    if meta.uid() != owner {
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

/// The directory holding `path` (`.` for a bare file name).
pub(crate) fn parent_of(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn validate_owner_dir(meta: &fs::Metadata) -> Result<(), SecureFileError> {
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

/// Take group and world write off `dir` when it is phux's own state
/// directory. A login umask of 002 leaves it group-writable whenever
/// something creates it with a plain `create_dir_all` before phux makes it
/// owner-only, so phux repairs its own directory rather than refuse to
/// start. Nobody else can create a file the effective user owns, and every
/// file in it is still checked on its own. Any other directory is one the
/// operator chose (`PHUX_WORKLOAD_*`) and is refused as it is. Best
/// effort: the check that follows reports what could not be repaired.
fn repair_own_state_dir(dir: &Path) {
    if dir == crate::telemetry::state_dir() {
        let _ = crate::owner_dir::tighten(dir);
    }
}

fn refuse_production(path: &Path) -> Result<(), SecureFileError> {
    phux_config::production::refuse_dev_on_production_state(path)
        .map_err(SecureFileError::ProductionState)
}

pub(crate) fn is_owned_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.file_type().is_file() && meta.uid() == effective_uid())
}

#[cfg(test)]
mod tests;
