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

/// Take group and world write off `dir`, as paired mode requires of the
/// state directory; one an older phux made under umask 002 has them. A
/// missing directory is left alone.
///
/// # Errors
///
/// The directory cannot be read or its mode changed.
pub fn tighten(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = match std::fs::metadata(dir) {
            Ok(meta) => meta.permissions().mode(),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        if mode & 0o022 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode & !0o022))?;
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
    fn a_group_writable_state_dir_loses_group_and_world_write() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("phux");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o775)).unwrap();
        super::tighten(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        super::tighten(&root.path().join("missing")).unwrap();
    }
}
