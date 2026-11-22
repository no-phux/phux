//! macOS only: which program the live server runs from, against the
//! `phux Server` bundle, and the privacy grants it holds (`PRIVACY_STATUS`).

use std::path::Path;

use phux_client::org::OrgError;
use phux_config::server_bundle;

use super::Check;
use crate::commands::cli_runtime;

/// The `server-bundle` and `permissions` checks; none off macOS, and none
/// when the server is unreachable (the `server` check says so).
pub(super) fn check_permissions(socket_path: &Path) -> Vec<Check> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let Ok(rt) = cli_runtime() else {
        return Vec::new();
    };
    match rt.block_on(phux_client::privacy::privacy_status(socket_path)) {
        Ok(report) => {
            let bundle = server_bundle::root()
                .filter(|root| server_bundle::current_executable(root).is_file());
            vec![
                bundle_check(&report, bundle.as_deref()),
                permissions_check(&report),
            ]
        }
        Err(OrgError::Unsupported) => vec![Check::warn(
            "permissions",
            "the running server predates PRIVACY_STATUS",
            "`phux upgrade` hands it to the installed binary without losing sessions",
        )],
        Err(_) => Vec::new(),
    }
}

/// Whether the live server runs from the bundle's `Current` path, where
/// grants given to `phux Server` reach it.
fn bundle_check(report: &serde_json::Value, bundle: Option<&Path>) -> Check {
    let exe = report["executable"].as_str().unwrap_or("an unknown path");
    let Some(root) = bundle else {
        return Check::pass(
            "server-bundle",
            format!("server runs from {exe}; no phux Server.app is installed"),
        );
    };
    let current = server_bundle::current_executable(root);
    if Path::new(exe) == current {
        Check::pass(
            "server-bundle",
            format!("server runs from the phux Server bundle ({exe})"),
        )
    } else {
        Check::warn(
            "server-bundle",
            format!(
                "server runs from {exe}, not the phux Server bundle at {}",
                current.display()
            ),
            "grants given to phux Server do not reach it. When the bundle holds this build, \
             `phux upgrade` moves the server into it; a bundle from another release is ignored \
             until `phux update` installs a matching one",
        )
    }
}

/// The three statuses, and what macOS charges them to.
fn permissions_check(report: &serde_json::Value) -> Check {
    let rows: Vec<(&str, &str)> = report["permissions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            (
                row["service"].as_str().unwrap_or("?"),
                row["status"].as_str().unwrap_or("unknown"),
            )
        })
        .collect();
    let mut detail = rows
        .iter()
        .map(|(service, status)| format!("{service} {status}"))
        .collect::<Vec<_>>()
        .join(", ");
    if let Some(charged) = report["responsible_path"].as_str() {
        detail = format!("{detail} (charged to {charged})");
    }
    if rows.iter().any(|(_, status)| *status != "granted") {
        detail.push_str("; grant one with `phux server permissions --open SERVICE`");
    }
    Check::pass("permissions", detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::Status;

    #[test]
    fn a_server_outside_the_bundle_is_flagged() {
        let root = Path::new("/Users/u/Library/Application Support/sh.phux/Server");
        let current = server_bundle::current_executable(root);
        let inside = serde_json::json!({"executable": current});
        assert_eq!(bundle_check(&inside, Some(root)).status, Status::Pass);
        let outside = serde_json::json!({"executable": "/Users/u/.local/bin/phux"});
        assert_eq!(bundle_check(&outside, Some(root)).status, Status::Warn);
        assert_eq!(bundle_check(&outside, None).status, Status::Pass);
    }

    #[test]
    fn the_permissions_line_names_every_status() {
        let report = serde_json::json!({
            "responsible_path": "/Applications/Ghostty.app",
            "permissions": [
                {"service": "full_disk_access", "status": "granted"},
                {"service": "screen_recording", "status": "denied"},
                {"service": "accessibility", "status": "unknown"},
            ],
        });
        let check = permissions_check(&report);
        assert_eq!(
            check.detail,
            "full_disk_access granted, screen_recording denied, accessibility unknown \
             (charged to /Applications/Ghostty.app); grant one with \
             `phux server permissions --open SERVICE`"
        );
    }
}
