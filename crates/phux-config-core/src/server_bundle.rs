//! The macOS server bundle, `phux Server.app`, at a stable path.
//!
//! macOS ties privacy grants (Full Disk Access, Screen Recording) to the
//! exact program that holds them, and every shell in a pane is attributed to
//! the server. A bare binary that moves or is rebuilt loses its grants, so
//! the installer and `phux update` keep a signed bundle under
//! `~/Library/Application Support/sh.phux/Server`:
//!
//! ```text
//! Versions/<release>/phux Server.app
//! Versions/Current -> <release>
//! ```
//!
//! Everything that starts a server (auto-spawn, the launchd unit, the
//! upgrade re-exec) names the `Current` path, never a version directory, so
//! a grant given to "phux Server" survives updates. Without the bundle
//! (Linux, source builds, Homebrew) the binary that would have run, runs.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The bundle's `CFBundleIdentifier`.
pub const BUNDLE_ID: &str = "sh.phux.server";

/// The bundle's directory name inside a version directory.
pub const APP_NAME: &str = "phux Server.app";

/// The bundle root under `home`.
#[must_use]
pub fn root_in(home: &Path) -> PathBuf {
    home.join("Library/Application Support/sh.phux/Server")
}

/// This user's bundle root; `None` off macOS or without `$HOME`.
#[must_use]
pub fn root() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| root_in(Path::new(&home)))
}

/// `Versions/Current/phux Server.app` under `root`: the path to grant
/// permissions to.
#[must_use]
pub fn current_app(root: &Path) -> PathBuf {
    root.join("Versions/Current").join(APP_NAME)
}

/// The server executable inside [`current_app`].
#[must_use]
pub fn current_executable(root: &Path) -> PathBuf {
    current_app(root).join("Contents/MacOS/phux")
}

/// The program to run as the server in place of `candidate`.
///
/// `candidate` is the binary that would otherwise run. The bundle's
/// `Current` executable wins when it is installed and holds the same build.
/// A development build never runs the installed bundle.
#[must_use]
pub fn server_executable(candidate: PathBuf) -> PathBuf {
    if crate::instance::is_dev_build() {
        return candidate;
    }
    match root() {
        Some(root) => choose(&root, candidate, same_build),
        None => candidate,
    }
}

/// [`server_executable`] for this running binary: what a server started
/// from it, or a unit naming it, runs.
///
/// # Errors
///
/// Whatever [`crate::instance::running_executable`] returns.
pub fn server_exe() -> std::io::Result<PathBuf> {
    crate::instance::running_executable().map(server_executable)
}

/// [`server_executable`]'s rule under `root`, with the build comparison
/// injected.
///
/// A `candidate` already inside the bundle (a server the bundle started,
/// re-exec'ing) always goes back through `Current`; any other must be the
/// same build, so a stale bundle never stands in for a newer binary.
pub fn choose(
    root: &Path,
    candidate: PathBuf,
    same_build: impl FnOnce(&Path, &Path) -> bool,
) -> PathBuf {
    let bundled = current_executable(root);
    if !bundled.is_file() {
        return candidate;
    }
    if candidate.starts_with(root) || same_build(&candidate, &bundled) {
        bundled
    } else {
        candidate
    }
}

/// Whether two phux binaries print the same `--version`.
fn same_build(a: &Path, b: &Path) -> bool {
    let version = |path: &Path| {
        Command::new(path)
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| output.stdout)
    };
    matches!((version(a), version(b)), (Some(x), Some(y)) if x == y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(root: &Path) {
        let exe = current_executable(root);
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"phux").unwrap();
    }

    #[test]
    fn the_current_bundle_wins_only_for_the_same_build() {
        let home = tempfile::tempdir().unwrap();
        let root = root_in(home.path());
        let cli = PathBuf::from("/Users/u/.local/bin/phux");

        // No bundle: whatever would have run, runs, without probing.
        assert_eq!(choose(&root, cli.clone(), |_, _| unreachable!()), cli);

        bundle(&root);
        let current = root.join("Versions/Current/phux Server.app/Contents/MacOS/phux");
        assert_eq!(choose(&root, cli.clone(), |_, _| true), current);
        assert_eq!(choose(&root, cli.clone(), |_, _| false), cli);

        // A server already inside the bundle (even a resolved version path)
        // re-execs through Current.
        let resolved = root.join("Versions/v1.2.3/phux Server.app/Contents/MacOS/phux");
        assert_eq!(choose(&root, resolved, |_, _| false), current);
    }
}
