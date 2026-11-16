//! Stat-stable, no-follow, owner-checked reads.

use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use super::{
    FileRole, SecureFileError, Stamp, insecure, parent_of, refuse_production, validate_owner_dir,
    validate_owner_file,
};
use crate::workload::scrub;

/// Largest file this module reads.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Attempts at reading one stable generation before calling it unstable.
const STABLE_READ_ATTEMPTS: usize = 4;

/// Stat `path` without following links and require owner-only control of it
/// and its directory. `Ok(None)` if absent.
pub(crate) fn probe(path: &Path, role: FileRole) -> Result<Option<Stamp>, SecureFileError> {
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
fn read_owner_file(
    path: &Path,
    role: FileRole,
) -> Result<Option<(Stamp, Vec<u8>)>, SecureFileError> {
    // The CA key is the one secret here: a dev build holding production's
    // could issue credentials the production server trusts.
    if matches!(role, FileRole::CaPrivateKey) {
        refuse_production(path)?;
    }
    let Some(before) = probe(path, role)? else {
        return Ok(None);
    };
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(SecureFileError::Unstable);
        }
        Err(error) => return Err(error.into()),
    };
    let opened = file.metadata()?;
    validate_owner_file(&opened, role)?;
    let stamp = Stamp::of(&opened);
    if !stamp.same_inode(&before) {
        return Err(SecureFileError::Unstable);
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
            .map_or(SecureFileError::Unstable, SecureFileError::from));
    }
    Ok(Some((stamp, raw)))
}

/// Read one stat-stable generation of `path`, retrying a bounded number of
/// times. `Ok(None)` if absent.
pub(crate) fn read_stable(
    path: &Path,
    role: FileRole,
) -> Result<Option<(Stamp, Vec<u8>)>, SecureFileError> {
    for _ in 0..STABLE_READ_ATTEMPTS {
        let read = match read_owner_file(path, role) {
            Err(SecureFileError::Unstable) => continue,
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
    Err(SecureFileError::Unstable)
}

/// Drop a read that did not become the answer, scrubbing its bytes first.
fn discard(read: Option<(Stamp, Vec<u8>)>) {
    if let Some((_, mut raw)) = read {
        scrub(&mut raw);
    }
}
