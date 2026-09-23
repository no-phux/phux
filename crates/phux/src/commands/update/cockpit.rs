//! Phux Cockpit rides along: `phux update` and `phux channel` keep an
//! installed Cockpit on the same channel as the CLI.
//!
//! The work is the driver the app's own Check for Updates runs
//! (`scripts/cockpit-self-update.sh`, which installs through
//! `scripts/install-cockpit.sh`), so there is still one Cockpit download and
//! verification stack. This binary embeds both scripts instead of running the
//! copies inside the bundle: a Cockpit built before channels existed can then
//! still be switched, and a `next` CLI always carries the newest installer.
//! The driver owns every refusal (Homebrew, Nix, dev builds, unknown layouts).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::channel::Channel;

const DRIVER: &str = include_str!("../../../../../scripts/cockpit-self-update.sh");
const INSTALLER: &str = include_str!("../../../../../scripts/install-cockpit.sh");

/// The driver's `key: value` document, as far as the CLI reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Report {
    /// `current`, `newer`, `installed`, `refused`, or `failed`.
    pub(crate) status: String,
    pub(crate) source: String,
    pub(crate) channel: String,
    pub(crate) current: String,
    pub(crate) latest: String,
    pub(crate) message: String,
    pub(crate) remedy: String,
}

impl Report {
    fn failed(message: String) -> Self {
        Self {
            status: "failed".to_owned(),
            message,
            ..Self::default()
        }
    }

    /// Parse the driver's stdout. A driver that died before its document
    /// (`die` in the script) leaves `status` empty; the caller supplies the
    /// failure from stderr.
    pub(crate) fn parse(text: &str) -> Self {
        let mut report = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line
                .split_once(": ")
                .or_else(|| line.strip_suffix(':').map(|key| (key, "")))
            else {
                continue;
            };
            let slot = match key {
                "status" => &mut report.status,
                "source" => &mut report.source,
                "channel" => &mut report.channel,
                "current" => &mut report.current,
                "latest" => &mut report.latest,
                "message" => &mut report.message,
                "remedy" => &mut report.remedy,
                _ => continue,
            };
            value.clone_into(slot);
        }
        report
    }

    pub(crate) fn document(&self, app: &Path) -> serde_json::Value {
        let optional = |value: &str| (!value.is_empty()).then(|| value.to_owned());
        serde_json::json!({
            "app": app.display().to_string(),
            "status": self.status,
            "source": optional(&self.source),
            "channel": optional(&self.channel),
            "current_version": optional(&self.current),
            "latest_version": optional(&self.latest),
            "message": self.message,
            "remedy": optional(&self.remedy),
        })
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!("cockpit:  {}", self.message)];
        if self.status == "newer" {
            lines.push("  run `phux update` to install it".to_owned());
        }
        if self.status == "installed" {
            lines.push("  quit and reopen Phux Cockpit if it is running".to_owned());
        }
        if !self.remedy.is_empty() {
            lines.push(format!("  {}", self.remedy));
        }
        lines
    }
}

/// Check (or, with `install`, move) `app` onto `channel`.
pub(crate) fn sync(app: &Path, channel: Channel, install: bool, bin_dir: Option<&Path>) -> Report {
    let scratch = match Scratch::create() {
        Ok(scratch) => scratch,
        Err(err) => return Report::failed(format!("could not stage the Cockpit updater: {err}")),
    };
    let mut command = Command::new("/bin/sh");
    command
        .arg(scratch.driver())
        .arg(if install { "--install" } else { "--check" })
        .arg("--channel")
        .arg(channel.as_str())
        .arg("--bundle")
        .arg(app)
        .arg("--installer")
        .arg(scratch.installer());
    if let Some(dir) = bin_dir {
        command.arg("--bin-dir").arg(dir);
    }
    let output = match command.output() {
        Ok(output) => output,
        Err(err) => return Report::failed(format!("could not run the Cockpit updater: {err}")),
    };
    let report = Report::parse(&String::from_utf8_lossy(&output.stdout));
    if !report.status.is_empty() {
        return report;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map_or("the Cockpit updater produced no report", |line| {
            line.trim_start_matches("error: ")
        });
    Report::failed(reason.to_owned())
}

/// The two embedded scripts, written to a private directory for one run.
struct Scratch(PathBuf);

impl Scratch {
    fn create() -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "phux-cockpit-update-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir(&dir)?;
        let scratch = Self(dir);
        // The driver execs the installer by path, so both must be executable.
        for (path, text) in [(scratch.driver(), DRIVER), (scratch.installer(), INSTALLER)] {
            fs::write(&path, text)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(scratch)
    }

    fn driver(&self) -> PathBuf {
        self.0.join("cockpit-self-update.sh")
    }

    fn installer(&self) -> PathBuf {
        self.0.join("install-cockpit.sh")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use std::path::Path;

    use super::{DRIVER, INSTALLER, Report};

    #[test]
    fn parse_reads_the_driver_document_and_ignores_the_rest() {
        let report = Report::parse(
            "status: newer\nsource: direct-release\nchannel: next\n\
             current: 0.29.0\nlatest: 0.29.0+next.0123456\n\
             applications_dir: /Applications\nrelaunch: no\n\
             message: Switching Phux Cockpit to the next channel installs 0.29.0+next.0123456 (you have 0.29.0).\n\
             remedy: \n",
        );
        assert_eq!(report.status, "newer");
        assert_eq!(report.channel, "next");
        assert_eq!(report.latest, "0.29.0+next.0123456");
        assert!(report.remedy.is_empty());
        let doc = report.document(Path::new("/Applications/Phux Cockpit.app"));
        assert_eq!(doc["status"], "newer");
        assert_eq!(doc["remedy"], serde_json::Value::Null);
        assert!(report.lines()[1].contains("phux update"));
    }

    #[test]
    fn a_refusal_carries_the_native_command() {
        let report = Report::parse(
            "status: refused\nsource: homebrew\n\
             message: This copy was installed with Homebrew. Use brew to update it.\n\
             remedy: brew upgrade --cask no-phux/tap/phux-cockpit\n",
        );
        assert_eq!(report.status, "refused");
        assert_eq!(
            report.lines().last().unwrap(),
            "  brew upgrade --cask no-phux/tap/phux-cockpit"
        );
    }

    #[test]
    fn a_driver_that_died_early_has_no_status() {
        assert!(Report::parse("").status.is_empty());
    }

    #[test]
    fn the_embedded_scripts_speak_channels() {
        assert!(DRIVER.contains("--channel <stable|latest|next>"));
        assert!(INSTALLER.contains("cockpit-channel.json"));
    }
}
