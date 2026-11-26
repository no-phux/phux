//! Directories phux makes for its own state, owner-only from the start.
//!
//! A login umask of 002 (stock Ubuntu) would otherwise leave them
//! group-writable, and the workload store refuses those.

use std::io;
use std::path::Path;

/// Create `dir` and any missing parents, mode 0700 on Unix. One that
/// already exists is left as it is.
///
/// # Errors
///
/// The directory cannot be created.
pub fn create(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Take every group and world bit off `dir`, one of phux's own directories,
/// as a new one starts.
///
/// One made by an older phux, or by a plain `create_dir_all` of something
/// inside it, is 0755 or, under umask 002, 0775. Paired mode refuses the
/// second and conversations refuse both. A missing directory, or one another
/// user owns, is left alone; a symbolic link is not followed.
///
/// # Errors
///
/// The directory cannot be opened or its mode changed.
pub fn tighten(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
        // Change the directory that was opened, not whatever the path
        // names a moment later.
        let handle = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW)
            .open(dir)
        {
            Ok(handle) => handle,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        let meta = handle.metadata()?;
        let mode = meta.permissions().mode();
        // Root in another user's home must not lock them out of it.
        if mode & 0o077 != 0 && meta.uid() == nix::unistd::geteuid().as_raw() {
            handle.set_permissions(std::fs::Permissions::from_mode(mode & !0o077))?;
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn a_new_state_dir_and_its_parents_are_owner_only() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("state/phux");
        super::create(&dir).unwrap();
        for dir in [root.path().join("state"), dir] {
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", dir.display());
        }
    }

    #[test]
    fn a_state_dir_others_can_open_becomes_owner_only() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("phux");
        std::fs::create_dir(&dir).unwrap();
        let mode = || std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        for loose in [0o755, 0o775] {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(loose)).unwrap();
            super::tighten(&dir).unwrap();
            assert_eq!(mode(), 0o700, "from {loose:o}");
        }
        super::tighten(&root.path().join("missing")).unwrap();

        // A link's target may be anyone's directory.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(super::tighten(&link).is_err());
        assert_eq!(mode(), 0o755);
    }
}
