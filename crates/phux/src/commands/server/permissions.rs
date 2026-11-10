//! `phux server permissions`: the macOS privacy grants the running server
//! holds (`PRIVACY_STATUS`), and `--open` for the System Settings pane that
//! grants one (`docs/install.md#macos-permissions`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::org::OrgError;
use phux_config::server_bundle;
use phux_server::runtime::default_socket_path;

use crate::commands::cli_runtime;
use crate::commands::json_err::{self, CliError, codes};

/// A privacy service `--open` can show. The JSON names are accepted too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, usage::ValueEnum)]
pub(crate) enum PrivacyService {
    /// Full Disk Access.
    #[usage(alias = "full_disk_access")]
    FullDiskAccess,
    /// Screen Recording.
    #[usage(alias = "screen_recording")]
    ScreenRecording,
    /// Accessibility.
    Accessibility,
}

impl PrivacyService {
    /// The System Settings URL of this service's pane.
    const fn settings_url(self) -> &'static str {
        match self {
            Self::FullDiskAccess => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"
            }
            Self::ScreenRecording => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            }
            Self::Accessibility => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            }
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::FullDiskAccess => "Full Disk Access",
            Self::ScreenRecording => "Screen Recording",
            Self::Accessibility => "Accessibility",
        }
    }
}

/// `phux server permissions [--json] [--open SERVICE]`. Does not start a
/// server; `--open` works without one.
pub(crate) fn run(socket: Option<PathBuf>, json: bool, open: Option<PrivacyService>) -> ExitCode {
    let socket = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let status = rt.block_on(phux_client::privacy::privacy_status(&socket));
    let bundle =
        server_bundle::root().filter(|root| server_bundle::current_executable(root).is_file());
    match open {
        Some(service) => open_pane(service, json, status.ok().as_ref(), bundle.as_deref()),
        None => match status {
            Ok(report) => {
                print_report(json, report, bundle.as_deref());
                ExitCode::SUCCESS
            }
            Err(err) => report_error(json, &socket, err),
        },
    }
}

fn report_error(json: bool, socket: &Path, err: OrgError) -> ExitCode {
    match err {
        OrgError::Attach(err) => {
            json_err::report_no_server(json, &err, socket, "server permissions")
        }
        OrgError::Unsupported => json_err::emit(
            json,
            &CliError::new(
                codes::SERVER_TOO_OLD,
                "the running server predates PRIVACY_STATUS",
                "`phux upgrade` hands it to the installed binary without losing sessions",
            ),
            1,
        ),
        other => json_err::emit(
            json,
            &CliError::new(
                codes::TRANSPORT,
                format!("the server did not report its permissions: {other}"),
                "retry; `phux doctor` checks the server",
            ),
            1,
        ),
    }
}

/// Print the server's document, plus `bundle`: the `Current` bundle path
/// (or `null`) that grants should name.
fn print_report(json: bool, mut report: serde_json::Value, bundle: Option<&Path>) {
    let bundle_app = bundle.map(server_bundle::current_app);
    if json {
        if let Some(fields) = report.as_object_mut() {
            fields.insert("bundle".to_owned(), serde_json::json!(bundle_app));
        }
        outln!("{report}");
        return;
    }
    let text = |key: &str| report[key].as_str().unwrap_or("unknown").to_owned();
    outln!("server       {}", text("executable"));
    outln!("charged to   {}", text("responsible_path"));
    if let Some(app) = &bundle_app {
        outln!("bundle       {}", app.display());
    }
    for row in report["permissions"].as_array().into_iter().flatten() {
        outln!(
            "{:<18} {}",
            row["service"].as_str().unwrap_or("?"),
            row["status"].as_str().unwrap_or("unknown")
        );
    }
}

/// Open `service`'s System Settings pane and print the path to add there.
fn open_pane(
    service: PrivacyService,
    json: bool,
    report: Option<&serde_json::Value>,
    bundle: Option<&Path>,
) -> ExitCode {
    if !cfg!(target_os = "macos") {
        return json_err::emit(
            json,
            &CliError::new(
                codes::USAGE_ERROR,
                "System Settings privacy panes exist only on macOS",
                "on this platform the server needs no privacy grants",
            ),
            2,
        );
    }
    let url = service.settings_url();
    let opened = std::process::Command::new("open")
        .arg(url)
        .status()
        .is_ok_and(|status| status.success());
    let target = grant_target(report, bundle);
    if json {
        outln!(
            "{}",
            serde_json::json!({"service": service, "url": url, "opened": opened, "path": target})
        );
    } else {
        if !opened {
            warn!("could not open System Settings; open it at {url}");
        }
        match &target {
            Some(path) => outln!(
                "In Privacy & Security > {}, turn on (or add with +) and restart the server:\n  {}",
                service.label(),
                path.display()
            ),
            None => outln!(
                "In Privacy & Security > {}, add the phux server.",
                service.label()
            ),
        }
    }
    if opened {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The program a grant must name: what macOS charges the running server to
/// (through `Current` when that is the bundle), else the installed bundle.
fn grant_target(report: Option<&serde_json::Value>, bundle: Option<&Path>) -> Option<PathBuf> {
    let responsible = report
        .and_then(|report| report["responsible_path"].as_str())
        .map(PathBuf::from);
    match (responsible, bundle) {
        (Some(path), Some(root)) if path.starts_with(root) => {
            Some(server_bundle::current_app(root))
        }
        (Some(path), _) => Some(path),
        (None, Some(root)) => Some(server_bundle::current_app(root)),
        (None, None) => None,
    }
}

impl serde::Serialize for PrivacyService {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::FullDiskAccess => "full_disk_access",
            Self::ScreenRecording => "screen_recording",
            Self::Accessibility => "accessibility",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_grant_names_the_bundle_through_current() {
        let root = Path::new("/Users/u/Library/Application Support/sh.phux/Server");
        let current = server_bundle::current_app(root);
        let resolved = serde_json::json!({
            "responsible_path": format!("{}/Versions/v1.2.3/phux Server.app", root.display())
        });
        assert_eq!(
            grant_target(Some(&resolved), Some(root)),
            Some(current.clone())
        );
        // A server a terminal started is charged to that terminal.
        let terminal = serde_json::json!({"responsible_path": "/Applications/Ghostty.app"});
        assert_eq!(
            grant_target(Some(&terminal), Some(root)),
            Some(PathBuf::from("/Applications/Ghostty.app"))
        );
        assert_eq!(grant_target(None, Some(root)), Some(current));
        assert_eq!(grant_target(None, None), None);
    }
}
