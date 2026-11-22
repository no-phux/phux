//! Discovery of first-party executables shipped beside `phux`.

use std::path::{Path, PathBuf};

const fn mcp_executable_name() -> &'static str {
    if cfg!(windows) {
        "phux-mcp.exe"
    } else {
        "phux-mcp"
    }
}

/// Whether `path` is a regular file with an execute bit (any file off Unix).
pub(crate) fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Find `phux-mcp`, preferring the release companion beside this `phux`.
pub(crate) fn find_mcp(
    current_exe: Option<&Path>,
    path: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    if let Some(sibling) = current_exe
        .and_then(Path::parent)
        .map(|parent| parent.join(mcp_executable_name()))
        && is_executable(&sibling)
    {
        return Some(sibling);
    }

    path.and_then(|value| {
        std::env::split_paths(value)
            .map(|dir| dir.join(mcp_executable_name()))
            .find(|candidate| is_executable(candidate))
    })
}

/// Discover `phux-mcp` from the running process and its environment.
pub(crate) fn find_live_mcp() -> Option<PathBuf> {
    let current_exe = std::env::current_exe().ok();
    find_mcp(current_exe.as_deref(), std::env::var_os("PATH").as_deref())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixture assertions")]
mod tests {
    use std::fs;

    use super::*;

    fn make_executable(path: &Path) {
        fs::write(path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn sibling_mcp_wins_then_path_is_the_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        let path_bin = temp.path().join("path");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&path_bin).unwrap();
        let current = bin.join("phux");
        let sibling = bin.join(mcp_executable_name());
        let fallback = path_bin.join(mcp_executable_name());
        make_executable(&sibling);
        make_executable(&fallback);

        assert_eq!(
            find_mcp(Some(&current), Some(path_bin.as_os_str())),
            Some(sibling.clone())
        );
        fs::remove_file(sibling).unwrap();
        assert_eq!(
            find_mcp(Some(&current), Some(path_bin.as_os_str())),
            Some(fallback)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_non_executable_file_is_not_a_companion() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let candidate = temp.path().join(mcp_executable_name());
        fs::write(&candidate, "").unwrap();
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(find_mcp(None, Some(temp.path().as_os_str())), None);
    }
}
