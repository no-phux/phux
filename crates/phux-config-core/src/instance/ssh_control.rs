//! The directory phux keeps its own ssh master sockets in.

use std::path::PathBuf;

/// Where phux keeps its own ssh master sockets: `/tmp/phux-ssh-<uid>`.
///
/// Created `0700` when `create` is set. `None` when it is missing, or is not
/// a directory private to this user (a symlink, another owner, or any mode
/// but `0700`). Short on purpose: a control socket must fit `sun_path`
/// (104 bytes on macOS) with ssh's 40-character `%C` and the 17-byte suffix
/// ssh adds while binding, which a macOS `$TMPDIR` leaves no room for.
#[cfg(unix)]
#[must_use]
pub fn ssh_control_dir(create: bool) -> Option<PathBuf> {
    let uid = nix::unistd::getuid().as_raw();
    private_dir(ssh_control_path(uid), create, uid)
}

#[cfg(unix)]
fn ssh_control_path(uid: u32) -> PathBuf {
    PathBuf::from(format!("/tmp/phux-ssh-{uid}"))
}

/// Always `None`: ssh master sockets are a Unix feature, so ssh runs here
/// connect on their own.
#[cfg(not(unix))]
#[must_use]
pub const fn ssh_control_dir(_create: bool) -> Option<PathBuf> {
    None
}

#[cfg(unix)]
fn private_dir(dir: PathBuf, create: bool, uid: u32) -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    if create {
        // An existing directory, whoever made it, is checked below.
        let _ = std::fs::DirBuilder::new().mode(0o700).create(&dir);
    }
    std::fs::symlink_metadata(&dir)
        .is_ok_and(|meta| meta.is_dir() && meta.uid() == uid && meta.mode() & 0o777 == 0o700)
        .then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn the_ssh_control_dir_must_be_private_to_this_user() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().expect("tempdir");
        let uid = nix::unistd::getuid().as_raw();
        let dir = root.path().join("ssh");

        assert_eq!(private_dir(dir.clone(), false, uid), None, "not created");
        assert_eq!(private_dir(dir.clone(), true, uid), Some(dir.clone()));
        assert_eq!(
            std::fs::metadata(&dir).expect("made").permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(private_dir(dir.clone(), false, uid.wrapping_add(1)), None);

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(
            private_dir(dir.clone(), true, uid),
            None,
            "group/other access"
        );

        let link = root.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).expect("symlink");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        assert_eq!(private_dir(link, false, uid), None, "a symlink");
    }

    /// Fixed under `/tmp`, never under a runtime or temp dir that can be
    /// long, so the socket fits `sun_path` for any uid.
    #[test]
    #[cfg(unix)]
    fn an_ssh_control_socket_fits_sun_path_for_any_uid() {
        const SUN_PATH_MACOS: usize = 104;
        const PERCENT_C: usize = 40;
        const BIND_SUFFIX: usize = 17;
        let dir = ssh_control_path(u32::MAX);
        let socket = dir.as_os_str().len() + 1 + PERCENT_C + BIND_SUFFIX;
        assert!(
            socket < SUN_PATH_MACOS,
            "{} -> {socket} bytes",
            dir.display()
        );
    }
}
