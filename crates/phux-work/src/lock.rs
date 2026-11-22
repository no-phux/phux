//! One exclusive writer for the store file.

use std::fs::{self, OpenOptions};
use std::path::Path;

use crate::error::Error;

#[derive(Debug)]
pub(crate) struct FileLock {
    file: fs::File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self.file, rustix::fs::FlockOperation::Unlock);
    }
}

/// Lock `path`, creating it mode `0600`. `block` waits; otherwise a held
/// lock is [`Error::Busy`].
pub(crate) fn acquire(path: &Path, block: bool) -> Result<FileLock, Error> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
        set_private_dir(parent)?;
    }
    let file = create_private(path)?;
    let operation = if block {
        rustix::fs::FlockOperation::LockExclusive
    } else {
        rustix::fs::FlockOperation::NonBlockingLockExclusive
    };
    match rustix::fs::flock(&file, operation) {
        Ok(()) => Ok(FileLock { file }),
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Err(Error::Busy),
        Err(err) => Err(Error::Io(err.into())),
    }
}

pub(crate) fn create_private(path: &Path) -> Result<fs::File, Error> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    set_private_file(path)?;
    Ok(file)
}

pub(crate) fn set_private_dir(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn set_private_file(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
