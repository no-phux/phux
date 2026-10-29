//! Sidecar log rotation: stat the live file, and when it is past the
//! threshold copy it aside and truncate it in place.
//!
//! The child holds the live file `O_APPEND` on fds 1 and 2. Truncating (not
//! renaming) keeps that descriptor on the same inode, and `O_APPEND` makes
//! the next write land at the new end instead of a hole.

use std::path::{Path, PathBuf};

use phux_plugin::startup_log;

/// How often the supervisor stats each live log.
pub(super) const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// Roll `path` when it is at least `threshold` bytes, keeping
/// `max_generations` older copies. Returns whether a rotation happened.
pub(super) fn rotate_log(
    path: &Path,
    threshold: u64,
    max_generations: usize,
) -> std::io::Result<bool> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(false);
    };
    if meta.len() < threshold {
        return Ok(false);
    }
    for (from, to) in shift_plan(path, max_generations) {
        if from.exists() {
            std::fs::rename(&from, &to)?;
        }
    }
    if max_generations > 0 {
        let gen1 = startup_log::generation_path(path, 1);
        std::fs::copy(path, &gen1)?;
        harden(&gen1)?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)?;
    Ok(true)
}

fn shift_plan(base: &Path, max_generations: usize) -> Vec<(PathBuf, PathBuf)> {
    (1..max_generations)
        .rev()
        .map(|n| {
            (
                startup_log::generation_path(base, n),
                startup_log::generation_path(base, n + 1),
            )
        })
        .collect()
}

fn harden(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let file = std::fs::File::open(path)?;
    let mut perms = file.metadata()?.permissions();
    perms.set_mode(startup_log::LOG_FILE_MODE);
    std::fs::set_permissions(path, perms)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

    #[test]
    fn copy_then_truncate_keeps_the_inode_and_three_generations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web.log");
        std::fs::write(&path, b"one").unwrap();
        let inode = std::fs::metadata(&path).unwrap().ino();
        assert!(rotate_log(&path, 1, 3).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"");
        assert_eq!(
            std::fs::read(startup_log::generation_path(&path, 1)).unwrap(),
            b"one"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);

        std::fs::write(&path, b"two").unwrap();
        assert!(rotate_log(&path, 1, 3).unwrap());
        assert_eq!(
            std::fs::read(startup_log::generation_path(&path, 1)).unwrap(),
            b"two"
        );
        assert_eq!(
            std::fs::read(startup_log::generation_path(&path, 2)).unwrap(),
            b"one"
        );
        assert!(!rotate_log(&path, 100, 3).unwrap());
    }
}
