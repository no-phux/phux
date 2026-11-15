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
}
