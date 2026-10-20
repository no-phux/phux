//! The TUI's "a newer phux is available" toast.
//!
//! Self-update stays explicit (ADR-0074); the TUI only tells the user. The
//! check shells out to `phux update --check` in the background once per
//! attach (the `phux` crate depends on this one), is silent on failure, and
//! is cached for `CACHE_TTL`. Opt out with `PHUX_NO_UPDATE_CHECK` set to any
//! non-empty value: the background check does not run, and a cached notice
//! does not paint.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a check answer is reused before the next attach re-checks.
const CACHE_TTL: Duration = Duration::from_hours(6);

/// Non-empty value disables the check entirely.
const OPT_OUT_ENV: &str = "PHUX_NO_UPDATE_CHECK";

/// How long the child may run before the check is abandoned.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// The cache file, beside the rest of the client's state.
const CACHE_FILE: &str = "update-check.json";

/// A newer release the user can install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateNotice {
    /// The running version, labelled (e.g. `0.45.0+next.b0c6858`).
    pub current: String,
    /// The newer published version, labelled (e.g. `v0.46.0`).
    pub latest: String,
    /// The exact command that installs it — `phux update`, or the install's
    /// own package-manager command when phux does not own the files.
    pub command: String,
}

impl UpdateNotice {
    /// The toast title.
    pub const TITLE: &'static str = "Update";

    /// The toast body: where you are, and the one command to move.
    #[must_use]
    pub fn body(&self) -> Vec<String> {
        vec![
            format!(
                "phux {} is available; you have {}.",
                self.latest, self.current
            ),
            format!("Run `{}`.", self.command),
        ]
    }

    fn to_value(&self) -> serde_json::Value {
        serde_json::json!({
            "current": self.current,
            "latest": self.latest,
            "command": self.command,
        })
    }

    fn from_value(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            current: value.get("current")?.as_str()?.to_owned(),
            latest: value.get("latest")?.as_str()?.to_owned(),
            command: value.get("command")?.as_str()?.to_owned(),
        })
    }
}

/// The version this binary was built as.
///
/// Matches what `phux update --check` reports as `current_version`, so a cache
/// naming another build is stale.
const fn running_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Parse a `phux update --check --json` document into a notice.
///
/// `None` when there is nothing to announce (no update, or an unreadable
/// document).
#[must_use]
pub fn notice_from_document(json: &str) -> Option<UpdateNotice> {
    let doc: serde_json::Value = serde_json::from_str(json).ok()?;
    if doc
        .get("update_available")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return None;
    }
    let current = label_current(
        doc.get("current_version")?.as_str()?,
        doc.get("current_sha").and_then(serde_json::Value::as_str),
    );
    let latest = label_latest(doc.get("latest_version")?.as_str()?);
    let command = doc
        .get("install")
        .and_then(|install| install.get("native_command"))
        .and_then(serde_json::Value::as_str)
        .filter(|command| !command.is_empty())
        .unwrap_or("phux update")
        .to_owned();
    Some(UpdateNotice {
        current,
        latest,
        command,
    })
}

/// Label the running version.
///
/// `0.45.0` plus a baked sha reads as `0.45.0+next.b0c6858`; without the sha it
/// is just the version.
fn label_current(version: &str, sha: Option<&str>) -> String {
    sha.filter(|sha| !sha.is_empty()).map_or_else(
        || version.to_owned(),
        |sha| format!("{version}+next.{}", short_sha(sha)),
    )
}

/// Label the published version.
///
/// A `next.<40-hex>` tag is shortened to `next.<7>`; a stable `vX.Y.Z` is left
/// alone.
fn label_latest(tag: &str) -> String {
    tag.strip_prefix("next.")
        .map_or_else(|| tag.to_owned(), |sha| format!("next.{}", short_sha(sha)))
}

fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn value_opts_out(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

fn opted_out() -> bool {
    value_opts_out(std::env::var_os(OPT_OUT_ENV).as_deref())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The cache file beside the client's other state.
fn cache_path() -> PathBuf {
    phux_config::instance::state_dir().join(CACHE_FILE)
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Replace `path` atomically so a reader never sees a half-written cache.
fn write_json(path: &Path, value: &serde_json::Value) {
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, value.to_string()).is_err() {
        return;
    }
    let _ = std::fs::rename(&tmp, path);
}

/// Kick the background check.
///
/// Called once by the production attach entry (`run_from_start`), never by the
/// test seam, so no test reaches the network.
pub fn spawn_background_refresh() {
    if opted_out() {
        return;
    }
    tokio::spawn(async {
        refresh(&cache_path()).await;
    });
}

/// Refresh the cache when it is stale (age or build), then leave it for the
/// driver's tick to read.
async fn refresh(path: &Path) {
    let now = unix_now();
    let existing = read_json(path);
    let same_build = existing
        .as_ref()
        .and_then(|doc| doc.get("current"))
        .and_then(serde_json::Value::as_str)
        == Some(running_version());
    let fresh = existing
        .as_ref()
        .and_then(|doc| doc.get("checked_at"))
        .and_then(serde_json::Value::as_u64)
        .is_some_and(|at| now.saturating_sub(at) < CACHE_TTL.as_secs());
    if fresh && same_build {
        return;
    }

    let notice = run_check()
        .await
        .and_then(|json| notice_from_document(&json));
    // A version already announced stays announced across refreshes.
    let notified = existing
        .as_ref()
        .and_then(|doc| doc.get("notified"))
        .and_then(serde_json::Value::as_str)
        .filter(|old| notice.as_ref().is_some_and(|notice| notice.latest == *old))
        .map(str::to_owned);
    write_json(
        path,
        &serde_json::json!({
            "checked_at": now,
            "current": running_version(),
            "available": notice.as_ref().map(UpdateNotice::to_value),
            "notified": notified,
        }),
    );
}

/// Run `phux update --check --json` with the installed binary. `None` on any
/// failure or timeout; the notice is best-effort.
async fn run_check() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let fut = tokio::process::Command::new(exe)
        .args(["update", "--check", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(CHECK_TIMEOUT, fut).await.ok()?.ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

/// The notice to show now, once.
///
/// `None` when the process opted out (`PHUX_NO_UPDATE_CHECK`), when there is
/// no update, when the cached answer is stale (another build wrote it, or it
/// was already shown), or when nothing has been checked yet. Marks the
/// version announced so it is shown once. An opt-out does not consume the
/// cache: a later process without the variable can still announce it.
#[must_use]
pub fn take_pending_notice() -> Option<UpdateNotice> {
    take_pending_notice_at(&cache_path())
}

/// [`take_pending_notice`] against an explicit cache file, so tests can drive
/// it without the real state directory.
fn take_pending_notice_at(path: &Path) -> Option<UpdateNotice> {
    pending_notice(path, opted_out())
}

/// Read a pending notice. `suppress` hides a cached newer release instead of
/// only skipping the fetch that would have written the cache.
fn pending_notice(path: &Path, suppress: bool) -> Option<UpdateNotice> {
    if suppress {
        return None;
    }
    let doc = read_json(path)?;
    if doc.get("current").and_then(serde_json::Value::as_str) != Some(running_version()) {
        return None;
    }
    let notice = UpdateNotice::from_value(doc.get("available")?)?;
    if doc.get("notified").and_then(serde_json::Value::as_str) == Some(notice.latest.as_str()) {
        return None;
    }
    let mut updated = doc;
    updated["notified"] = serde_json::Value::String(notice.latest.clone());
    write_json(path, &updated);
    Some(notice)
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn document(update_available: bool) -> String {
        serde_json::json!({
            "schema_version": 1,
            "action": "checked",
            "channel": "next",
            "current_version": "0.45.0",
            "current_sha": "b0c685882bf746ea0e0748b61e9722de89590983",
            "latest_version": "next.d527f26b87a7af6a7c6391daa9ad92f2f482bf19",
            "target_version": "next.d527f26b87a7af6a7c6391daa9ad92f2f482bf19",
            "update_available": update_available,
            "install": {
                "source": "direct-release",
                "executable": "/home/u/.local/bin/phux",
                "mutable": true,
                "native_command": null,
            },
        })
        .to_string()
    }

    #[test]
    fn an_available_update_becomes_a_short_labelled_notice() {
        let notice = notice_from_document(&document(true)).expect("notice");
        assert_eq!(notice.current, "0.45.0+next.b0c6858");
        assert_eq!(notice.latest, "next.d527f26");
        assert_eq!(notice.command, "phux update");
        assert_eq!(UpdateNotice::TITLE, "Update");
        assert_eq!(
            notice.body(),
            vec![
                "phux next.d527f26 is available; you have 0.45.0+next.b0c6858.",
                "Run `phux update`."
            ]
        );
    }

    #[test]
    fn nothing_to_announce_yields_no_notice() {
        assert!(notice_from_document(&document(false)).is_none());
        assert!(notice_from_document("not json").is_none());
        assert!(notice_from_document("{}").is_none());
    }

    #[test]
    fn a_package_managed_install_names_its_native_command() {
        let mut doc: serde_json::Value = serde_json::from_str(&document(true)).expect("json");
        doc["install"]["source"] = serde_json::json!("homebrew");
        doc["install"]["native_command"] = serde_json::json!("brew upgrade no-phux/tap/phux");
        let notice = notice_from_document(&doc.to_string()).expect("notice");
        assert_eq!(notice.command, "brew upgrade no-phux/tap/phux");
    }

    fn seed(path: &Path, available: bool, notified: Option<&str>, current: &str) {
        write_json(
            path,
            &serde_json::json!({
                "checked_at": unix_now(),
                "current": current,
                "available": available.then(|| UpdateNotice {
                    current: "0.45.0+next.b0c6858".to_owned(),
                    latest: "next.d527f26".to_owned(),
                    command: "phux update".to_owned(),
                }.to_value()),
                "notified": notified,
            }),
        );
    }

    #[test]
    fn a_pending_notice_is_taken_once_and_then_silent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CACHE_FILE);
        seed(&path, true, None, running_version());

        let first = take_pending_notice_at(&path).expect("first notice");
        assert_eq!(first.latest, "next.d527f26");
        assert!(
            take_pending_notice_at(&path).is_none(),
            "the version is announced once, not on every attach"
        );
    }

    #[test]
    fn the_opt_out_is_any_non_empty_phux_no_update_check() {
        use std::ffi::OsStr;

        assert_eq!(OPT_OUT_ENV, "PHUX_NO_UPDATE_CHECK");
        assert!(value_opts_out(Some(OsStr::new("1"))));
        assert!(value_opts_out(Some(OsStr::new("true"))));
        assert!(!value_opts_out(Some(OsStr::new(""))));
        assert!(!value_opts_out(None));
    }

    #[test]
    fn an_opt_out_hides_a_cached_newer_release() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CACHE_FILE);
        seed(&path, true, None, running_version());

        assert!(
            pending_notice(&path, true).is_none(),
            "PHUX_NO_UPDATE_CHECK must suppress a cached notice, not only the fetch"
        );
        // The opt-out must not consume the announcement. With the check
        // enabled, the same cache still paints.
        let shown = pending_notice(&path, false).expect("shown when the check is enabled");
        assert_eq!(shown.latest, "next.d527f26");
        assert!(
            shown
                .body()
                .iter()
                .any(|line| line.contains("is available")),
            "the cached notice is what the toast would paint"
        );
    }

    #[test]
    fn no_update_or_a_foreign_build_yields_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(CACHE_FILE);

        seed(&path, true, None, "0.0.0-other");
        assert!(
            take_pending_notice_at(&path).is_none(),
            "a cache from another build is stale: the user may have updated"
        );

        seed(&path, false, None, running_version());
        assert!(take_pending_notice_at(&path).is_none());
    }
}
