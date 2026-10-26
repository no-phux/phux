//! Resolve the coordinator afresh, bootstrapping the signed app's CLI without
//! replacing an existing installation or modifying the app bundle.

use anyhow::Context as _;
use std::path::{Path, PathBuf};

pub(crate) const MISSING: &str = "The phux CLI is missing. Reinstall Phux Desktop, or install the CLI from https://phux.sh/install, then click Retry connection. Your sessions have not been changed.";

// One coordinator CLI is configured for the whole app process (as on WSL).
// Keep this bit separate from command text: explicit `--phux phux` means PATH,
// not automatic installed-location discovery or permission to bootstrap.
static EXPLICIT_COMMAND: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub(super) fn configure_explicit(explicit: bool) {
    let _ = EXPLICIT_COMMAND.set(explicit);
}

pub(crate) fn command_path(requested: &Path) -> anyhow::Result<PathBuf> {
    resolve_command(requested, *EXPLICIT_COMMAND.get().unwrap_or(&false))
}

fn resolve_command(requested: &Path, explicit: bool) -> anyhow::Result<PathBuf> {
    if requested.as_os_str().is_empty() {
        anyhow::bail!(MISSING);
    }
    if requested == Path::new("phux") && !explicit {
        find(None)
    } else {
        // Explicit paths never silently switch to a different installation.
        Ok(requested.to_path_buf())
    }
}

pub(super) fn find(explicit: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit.or_else(|| std::env::var_os("PHUX_BIN").map(PathBuf::from)) {
        return Ok(path);
    }
    #[cfg(windows)]
    return Ok(PathBuf::from("phux"));
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context(MISSING)?;
        let mut candidates = vec![
            home.join(".local/bin/phux"),
            PathBuf::from("/opt/homebrew/bin/phux"),
            PathBuf::from("/usr/local/bin/phux"),
            home.join(".cargo/bin/phux"),
        ];
        if let Some(path) = std::env::var_os("PATH") {
            candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("phux")));
        }
        let executable = std::env::current_exe().ok();
        resolve_candidates(
            &candidates,
            executable.as_deref(),
            &home.join(".local/bin/phux"),
        )
    }
}

#[cfg(not(windows))]
fn resolve_candidates(
    candidates: &[PathBuf],
    executable: Option<&Path>,
    destination: &Path,
) -> anyhow::Result<PathBuf> {
    if let Some(found) = candidates.iter().find(|path| executable_file(path)) {
        return Ok(found.clone());
    }
    let source = executable.and_then(bundled_cli).context(MISSING)?;
    install_bundled(&source, destination)?;
    Ok(destination.to_path_buf())
}

#[cfg(not(windows))]
fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(windows))]
fn bundled_cli(executable: &Path) -> Option<PathBuf> {
    let dir = executable.parent()?;
    if !dir.ends_with("Phux.app/Contents/MacOS") {
        return None;
    }
    let cli = dir.join("phux");
    executable_file(&cli).then_some(cli)
}

#[cfg(not(windows))]
fn install_bundled(source: &Path, destination: &Path) -> anyhow::Result<()> {
    use std::fs::{self, OpenOptions};
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = destination
        .parent()
        .context("No CLI installation directory")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "Could not create {}. Fix its permissions, then click Retry connection.",
            parent.display()
        )
    })?;
    let staged = parent.join(format!(
        ".phux-bootstrap-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&staged).with_context(|| format!("Could not stage the bundled CLI in {}. Check permissions, then click Retry connection.", parent.display()))?;
    let result =
        write_runtime(source, &mut output).and_then(|()| publish_runtime(&staged, destination));
    let _ = fs::remove_file(staged);
    result.with_context(|| format!("Could not install the bundled phux CLI at {}. Check that path and its permissions, then click Retry connection. No existing CLI was replaced.", destination.display()))
}

#[cfg(not(windows))]
fn write_runtime(source: &Path, output: &mut std::fs::File) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut input = std::fs::File::open(source)?;
    std::io::copy(&mut input, output)?;
    output.set_permissions(std::fs::Permissions::from_mode(0o755))?;
    output.sync_all()?;
    Ok(())
}

#[cfg(not(windows))]
fn publish_runtime(staged: &Path, destination: &Path) -> anyhow::Result<()> {
    // Linking into place is atomic and refuses to overwrite a concurrent install.
    match std::fs::hard_link(staged, destination) {
        Ok(()) => Ok(()),
        Err(error)
            if error.kind() == std::io::ErrorKind::AlreadyExists
                && executable_file(destination) =>
        {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "phux-bootstrap-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn file(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn dmg_only_bootstraps_the_bundled_cli_without_modifying_the_bundle() {
        let fixture = Fixture::new();
        let app = fixture.file("Phux.app/Contents/MacOS/phux-desktop", "desktop");
        let source = fixture.file("Phux.app/Contents/MacOS/phux", "bundled runtime");
        let destination = fixture.0.join("home/.local/bin/phux");
        assert_eq!(
            resolve_candidates(&[], Some(&app), &destination).unwrap(),
            destination
        );
        assert_eq!(fs::read_to_string(&destination).unwrap(), "bundled runtime");
        assert_eq!(fs::read_to_string(source).unwrap(), "bundled runtime");
        assert!(executable_file(&destination));
        assert_eq!(
            fs::read_dir(destination.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn an_existing_cli_wins_and_a_later_install_is_rediscovered() {
        let fixture = Fixture::new();
        let destination = fixture.0.join("bin/phux");
        let candidates = vec![destination.clone()];
        assert!(resolve_candidates(&candidates, None, &destination).is_err());
        fixture.file("bin/phux", "installed later");
        assert_eq!(
            resolve_candidates(&candidates, None, &destination).unwrap(),
            destination
        );
        assert_eq!(fs::read_to_string(destination).unwrap(), "installed later");
    }

    #[test]
    fn bootstrap_never_overwrites_an_existing_file_or_concurrent_install() {
        let fixture = Fixture::new();
        let source = fixture.file("source", "new");
        let destination = fixture.file("bin/phux", "old");
        install_bundled(&source, &destination).unwrap();
        assert_eq!(fs::read_to_string(&destination).unwrap(), "old");
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(install_bundled(&source, &destination).is_err());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "old");
        assert_eq!(
            fs::read_dir(destination.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn explicit_missing_paths_stay_pinned_and_non_bundles_cannot_bootstrap() {
        let missing = Path::new("/missing/explicit/phux");
        assert_eq!(command_path(missing).unwrap(), missing);
        assert_eq!(
            resolve_command(Path::new("phux"), true).unwrap(),
            Path::new("phux")
        );
        let fixture = Fixture::new();
        let app = fixture.file("target/phux-desktop", "desktop");
        fixture.file("target/phux", "not a bundled runtime");
        let destination = fixture.0.join("bin/phux");
        assert!(resolve_candidates(&[], Some(&app), &destination).is_err());
        assert!(!destination.exists());
    }
}
