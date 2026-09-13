//! `phux cockpit` — open the installed native macOS Cockpit app.
//!
//! Discovery is the same pair of locations the curl installer writes, plus
//! `PHUX_COCKPIT_APP` as an explicit override. Launch goes through
//! Launch Services (`open` with the bundle path, never `-a` by name) so a
//! second registered copy cannot steal the start.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use super::json_err::{self, CliError, codes};
use crate::exit_codes::{EXIT_FAILURE, EXIT_SUCCESS, EXIT_USAGE};

/// Version of the `phux cockpit --json` document. Additive fields do not bump it.
pub(crate) const DOCUMENT_SCHEMA_VERSION: u8 = 1;

const APP_NAME: &str = "Phux Cockpit.app";
const INSTALL_REMEDY: &str = "curl -fsSL https://phux.sh/install-cockpit | sh";
const OVERRIDE_ENV: &str = "PHUX_COCKPIT_APP";

/// Why Cockpit cannot be opened.
#[derive(Debug)]
enum LaunchError {
    Unsupported,
    NotInstalled,
    InvalidOverride(PathBuf),
    OpenFailed(io::Error),
}

impl LaunchError {
    fn into_cli(self) -> (CliError, u8) {
        match self {
            Self::Unsupported => (
                CliError::new(
                    codes::COCKPIT_UNSUPPORTED_PLATFORM,
                    "Phux Cockpit is macOS-only",
                    "install Cockpit on an Apple silicon Mac",
                ),
                EXIT_USAGE,
            ),
            Self::NotInstalled => (
                CliError::new(
                    codes::COCKPIT_NOT_INSTALLED,
                    "Phux Cockpit is not installed",
                    INSTALL_REMEDY,
                ),
                EXIT_FAILURE,
            ),
            Self::InvalidOverride(path) => (
                CliError::new(
                    codes::COCKPIT_INVALID_APP,
                    format!(
                        "{OVERRIDE_ENV} is not a Phux Cockpit.app bundle: {}",
                        path.display()
                    ),
                    format!(
                        "point {OVERRIDE_ENV} at Phux Cockpit.app, or unset it and {INSTALL_REMEDY}"
                    ),
                ),
                EXIT_USAGE,
            ),
            Self::OpenFailed(err) => (
                CliError::new(
                    codes::COCKPIT_LAUNCH_FAILED,
                    format!("could not open Phux Cockpit: {err}"),
                    "confirm the app is still in Applications, or reinstall with the curl installer",
                ),
                EXIT_FAILURE,
            ),
        }
    }
}

fn is_app_bundle(path: &Path) -> bool {
    path.is_dir() && path.join("Contents").join("Info.plist").is_file()
}

/// Directories that may contain `Phux Cockpit.app`.
fn application_roots(home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = home {
        roots.push(home.join("Applications"));
    }
    roots
}

/// Resolve the app bundle. An explicit override never falls through.
fn resolve_app(roots: &[PathBuf], override_app: Option<&Path>) -> Result<PathBuf, LaunchError> {
    if let Some(path) = override_app {
        if is_app_bundle(path) {
            return Ok(path.to_path_buf());
        }
        return Err(LaunchError::InvalidOverride(path.to_path_buf()));
    }
    roots
        .iter()
        .map(|root| root.join(APP_NAME))
        .find(|path| is_app_bundle(path))
        .ok_or(LaunchError::NotInstalled)
}

fn open_app(app: &Path) -> io::Result<()> {
    let status = Command::new("open").arg(app).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "open exited {}",
            status
                .code()
                .map_or_else(|| "by signal".to_owned(), |code| code.to_string())
        )))
    }
}

fn document(app: &Path) -> serde_json::Value {
    serde_json::json!({
        "schema_version": DOCUMENT_SCHEMA_VERSION,
        "app": app.display().to_string(),
        "launched": true,
    })
}

fn launch_with(
    macos: bool,
    roots: &[PathBuf],
    override_app: Option<&Path>,
    open: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<PathBuf, LaunchError> {
    if !macos {
        return Err(LaunchError::Unsupported);
    }
    let app = resolve_app(roots, override_app)?;
    open(&app).map_err(LaunchError::OpenFailed)?;
    Ok(app)
}

fn report(json: bool, result: Result<PathBuf, LaunchError>) -> ExitCode {
    match result {
        Ok(app) => {
            if json {
                outln!("{}", document(&app));
            } else {
                outln!("opened {}", app.display());
            }
            ExitCode::from(EXIT_SUCCESS)
        }
        Err(err) => {
            let (cli, code) = err.into_cli();
            json_err::emit(json, &cli, code)
        }
    }
}

/// Open the installed Cockpit app.
pub(crate) fn run(json: bool) -> ExitCode {
    let override_app = std::env::var_os(OVERRIDE_ENV).map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let roots = application_roots(home.as_deref());
    report(
        json,
        launch_with(
            cfg!(target_os = "macos"),
            &roots,
            override_app.as_deref(),
            open_app,
        ),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixture assertions")]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    use super::*;

    fn make_bundle(root: &Path) -> PathBuf {
        let app = root.join(APP_NAME);
        fs::create_dir_all(app.join("Contents")).unwrap();
        fs::write(app.join("Contents").join("Info.plist"), "plist\n").unwrap();
        app
    }

    #[test]
    fn override_wins_and_never_falls_through() {
        let temp = tempfile::tempdir().unwrap();
        let home_apps = temp.path().join("home");
        let override_root = temp.path().join("override");
        fs::create_dir_all(home_apps.join("Applications")).unwrap();
        let home_app = make_bundle(&home_apps.join("Applications"));
        let override_app = make_bundle(&override_root);
        let roots = [home_apps.join("Applications")];

        assert_eq!(
            resolve_app(&roots, Some(&override_app)).unwrap(),
            override_app
        );
        assert!(matches!(
            resolve_app(&roots, Some(temp.path())),
            Err(LaunchError::InvalidOverride(_))
        ));
        assert_eq!(resolve_app(&roots, None).unwrap(), home_app);
    }

    #[test]
    fn missing_bundle_is_not_installed() {
        let temp = tempfile::tempdir().unwrap();
        let roots = [temp.path().to_path_buf()];
        assert!(matches!(
            resolve_app(&roots, None),
            Err(LaunchError::NotInstalled)
        ));
    }

    #[test]
    fn a_directory_without_info_plist_is_not_an_app() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(APP_NAME)).unwrap();
        let roots = [temp.path().to_path_buf()];
        assert!(matches!(
            resolve_app(&roots, None),
            Err(LaunchError::NotInstalled)
        ));
    }

    #[test]
    fn launch_refuses_non_macos_before_touching_the_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let app = make_bundle(temp.path());
        let err = launch_with(false, &[], Some(&app), |_| {
            panic!("open must not run off macOS")
        })
        .unwrap_err();
        assert!(matches!(err, LaunchError::Unsupported));
    }

    #[test]
    fn launch_opens_the_resolved_bundle_path() {
        let temp = tempfile::tempdir().unwrap();
        let app = make_bundle(temp.path());
        let opened = Mutex::new(None);
        let launched = launch_with(true, &[], Some(&app), |path| {
            *opened.lock().unwrap() = Some(path.to_path_buf());
            Ok(())
        })
        .unwrap();
        assert_eq!(launched, app);
        assert_eq!(opened.lock().unwrap().as_deref(), Some(app.as_path()));
    }

    #[test]
    fn success_document_is_versioned() {
        let doc = document(Path::new("/Applications/Phux Cockpit.app"));
        assert_eq!(doc["schema_version"], u64::from(DOCUMENT_SCHEMA_VERSION));
        assert_eq!(doc["app"], "/Applications/Phux Cockpit.app");
        assert_eq!(doc["launched"], serde_json::json!(true));
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(
            codes::COCKPIT_UNSUPPORTED_PLATFORM,
            "cockpit_unsupported_platform"
        );
        assert_eq!(codes::COCKPIT_NOT_INSTALLED, "cockpit_not_installed");
        assert_eq!(codes::COCKPIT_INVALID_APP, "cockpit_invalid_app");
        assert_eq!(codes::COCKPIT_LAUNCH_FAILED, "cockpit_launch_failed");
    }
}
