//! Release checks and explicit updates. No downloaded code runs during a check.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Channel {
    #[default]
    Stable,
    Next,
}

impl Channel {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Next => "next",
        }
    }
    fn parse(value: &Value) -> Self {
        if value == "next" {
            Self::Next
        } else {
            Self::Stable
        }
    }
}

#[derive(Clone, Debug)]
pub struct Preferences {
    pub automatic: bool,
    pub desktop: Channel,
    pub runtime: Channel,
    pub manage_runtime: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            automatic: true,
            desktop: Channel::Stable,
            runtime: Channel::Stable,
            manage_runtime: false,
        }
    }
}

impl Preferences {
    pub fn from_value(value: &Value) -> Self {
        Self {
            automatic: value["automatic"].as_bool().unwrap_or(true),
            desktop: Channel::parse(&value["desktop"]),
            runtime: Channel::parse(&value["runtime"]),
            manage_runtime: value["manage_runtime"].as_bool().unwrap_or(false),
        }
    }
    pub fn load(channel: Channel) -> Self {
        std::fs::read(config_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .map(|value| Self::from_value(&value))
            .unwrap_or_else(|| Self {
                desktop: channel,
                ..Self::default()
            })
    }
    pub fn save(&self) -> anyhow::Result<()> {
        let path = config_path();
        std::fs::create_dir_all(path.parent().context("No update configuration directory")?)?;
        let staged = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(
            &staged,
            serde_json::to_vec_pretty(&json!({
                "automatic": self.automatic, "desktop": self.desktop.name(),
                "runtime": self.runtime.name(), "manage_runtime": self.manage_runtime,
            }))?,
        )?;
        std::fs::rename(staged, path)?;
        Ok(())
    }
}

fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("phux-desktop/updates.json")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub revision: String,
    pub channel: Channel,
}

/// Only the installed, standard bundle can replace itself; bare development
/// executables must never turn a test into an installation.
pub fn app_bundle(executable: &Path) -> Option<PathBuf> {
    if !executable.ends_with("Phux.app/Contents/MacOS/phux-desktop") {
        return None;
    }
    Some(executable.parent()?.parent()?.parent()?.to_path_buf())
}

pub fn installed_release(bundle: &Path) -> anyhow::Result<Release> {
    let read = |key: &str| -> anyhow::Result<String> {
        output(
            Command::new("/usr/libexec/PlistBuddy")
                .args(["-c", &format!("Print :{key}")])
                .arg(bundle.join("Contents/Info.plist")),
        )
    };
    Ok(Release {
        version: read("PhuxDesktopVersion")?.trim().to_owned(),
        revision: read("PhuxBuildSHA")?.trim().to_owned(),
        channel: if read("PhuxDesktopChannel").unwrap_or_default().trim() == "next" {
            Channel::Next
        } else {
            Channel::Stable
        },
    })
}

fn fetch(url: &str) -> anyhow::Result<Value> {
    let text = output(Command::new("/usr/bin/curl").args([
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--proto",
        "=https",
        "--connect-timeout",
        "10",
        "--max-time",
        "30",
        "--max-filesize",
        "8388608",
        url,
    ]))?;
    serde_json::from_str(&text).context("Invalid release metadata")
}

fn stable_version(tag: &str) -> Option<(u64, u64, u64)> {
    let mut parts = tag.strip_prefix("desktop-v")?.split('.');
    let number = |part: &str| {
        let value: u64 = part.parse().ok()?;
        (part == value.to_string()).then_some(value)
    };
    let version = (
        number(parts.next()?)?,
        number(parts.next()?)?,
        number(parts.next()?)?,
    );
    parts.next().is_none().then_some(version)
}

pub fn stable_releases(value: &Value) -> anyhow::Result<Vec<Release>> {
    let rows = value.as_array().context("Expected a release list")?;
    Ok(rows
        .iter()
        .filter(|row| row["draft"] == false && row["prerelease"] == false)
        .filter_map(|row| {
            let tag = row["tag_name"].as_str()?;
            stable_version(tag)?;
            Some(Release {
                version: tag.strip_prefix("desktop-v")?.to_owned(),
                revision: String::new(),
                channel: Channel::Stable,
            })
        })
        .collect())
}

pub fn latest(channel: Channel) -> anyhow::Result<Release> {
    if channel == Channel::Next {
        return next_release(&fetch(
            "https://github.com/no-phux/phux/releases/download/next/desktop-channel.json",
        )?);
    }
    let mut releases = Vec::new();
    for page in 1..=10 {
        let value = fetch(&format!(
            "https://api.github.com/repos/no-phux/phux/releases?per_page=100&page={page}"
        ))?;
        releases.extend(stable_releases(&value)?);
        if value.as_array().is_some_and(|rows| rows.len() < 100) {
            break;
        }
    }
    releases
        .into_iter()
        .max_by_key(|release| stable_version(&format!("desktop-v{}", release.version)))
        .context("No published Stable desktop release")
}

pub fn next_release(value: &Value) -> anyhow::Result<Release> {
    if value["schema_version"] != 1 || value["product"] != "desktop" || value["channel"] != "next" {
        bail!("Invalid Desktop Next channel metadata");
    }
    let revision = value["sha"]
        .as_str()
        .context("Desktop Next channel has no revision")?;
    if !valid_revision(revision) {
        bail!("Invalid Desktop Next revision");
    }
    let version = value["version"]
        .as_str()
        .context("Desktop Next channel has no version")?;
    if stable_version(&format!("desktop-v{version}")).is_none() {
        bail!("Invalid Desktop Next version");
    }
    if value["archive"] != format!("phux-desktop-next.{revision}-macos-arm64.zip") {
        bail!("Desktop Next archive does not match its revision");
    }
    Ok(Release {
        version: version.into(),
        revision: revision.into(),
        channel: Channel::Next,
    })
}

fn valid_revision(revision: &str) -> bool {
    revision.len() == 40
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

pub fn available(current: &Release, target: &Release) -> bool {
    if current.channel != target.channel {
        return true;
    }
    if target.channel == Channel::Next {
        return current.revision != target.revision;
    }
    stable_version(&format!("desktop-v{}", target.version))
        > stable_version(&format!("desktop-v{}", current.version))
}

#[derive(Clone, Debug)]
pub struct RuntimeRelease {
    pub current: String,
    pub target: String,
    pub available: bool,
    pub mutable: bool,
    pub native_command: Option<String>,
}

pub fn runtime_check(phux: &Path, channel: Channel) -> anyhow::Result<RuntimeRelease> {
    let text = check_output(Command::new(phux).args([
        "update",
        "--check",
        "--channel",
        channel.name(),
        "--json",
    ]))?;
    let value: Value = serde_json::from_str(&text)?;
    if value["schema_version"] != 1 {
        bail!("Unsupported phux update response");
    }
    Ok(RuntimeRelease {
        current: value["current_version"]
            .as_str()
            .context("Missing current version")?
            .into(),
        target: value["target_version"]
            .as_str()
            .context("Missing target version")?
            .into(),
        available: value["update_available"]
            .as_bool()
            .context("Missing update availability")?,
        mutable: value["install"]["mutable"].as_bool().unwrap_or(false),
        native_command: value["install"]["native_command"]
            .as_str()
            .map(str::to_owned),
    })
}

pub struct RuntimeInstalled {
    pub message: String,
    pub reconnect: bool,
    pub retry_handoff: bool,
}

pub fn runtime_result(value: &Value) -> anyhow::Result<RuntimeInstalled> {
    if value["schema_version"] != 1 {
        bail!("Unsupported phux update response");
    }
    let result = value["server_handoff"]["result"]
        .as_str()
        .unwrap_or("not attempted");
    Ok(RuntimeInstalled {
        message: format!(
            "phux installed. Server handoff: {result}. {}",
            value["server_handoff"]["detail"].as_str().unwrap_or("")
        ),
        reconnect: matches!(result, "upgrading" | "no_server"),
        retry_handoff: matches!(result, "refused" | "failed"),
    })
}

pub fn runtime_install(
    phux: &Path,
    socket: &Path,
    channel: Channel,
) -> anyhow::Result<RuntimeInstalled> {
    let text = output(
        Command::new(phux)
            .arg("--socket")
            .arg(socket)
            .args(["update", "--channel", channel.name(), "--json"])
            .stdin(Stdio::null()),
    )?;
    let value: Value = serde_json::from_str(&text)?;
    let mut result = runtime_result(&value)?;
    if value["server_handoff"]["result"] == "no_server" {
        output(
            Command::new(phux)
                .arg("--socket")
                .arg(socket)
                .args(["server", "--ensure"]),
        )
        .context("phux was installed but the server could not start")?;
    }
    if !result.reconnect {
        result.message.push_str(&format!(
            " Retry the handoff with {} --socket {} upgrade. No agent sessions were closed.",
            crate::attach::quote_paths(&[phux.to_path_buf()]),
            crate::attach::quote_paths(&[socket.to_path_buf()])
        ));
    }
    Ok(result)
}

pub fn runtime_retry(phux: &Path, socket: &Path) -> anyhow::Result<RuntimeInstalled> {
    output(
        Command::new(phux)
            .arg("--socket")
            .arg(socket)
            .arg("upgrade")
            .stdin(Stdio::null()),
    )?;
    Ok(RuntimeInstalled {
        message: "Server handoff requested; reconnecting windows.".into(),
        reconnect: true,
        retry_handoff: false,
    })
}

pub fn desktop_install(bundle: &Path, target: &Release) -> anyhow::Result<()> {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-s")
        .arg("--")
        .arg("--channel")
        .arg(target.channel.name())
        .arg("--applications-dir")
        .arg(bundle.parent().context("No Applications directory")?);
    if target.channel == Channel::Stable {
        command.arg("--version").arg(&target.version);
    }
    if target.channel == Channel::Next {
        command.arg("--revision").arg(&target.revision);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    use std::io::Write;
    child
        .stdin
        .take()
        .context("No installer input")?
        .write_all(include_bytes!("../../../../scripts/install-desktop.sh"))?;
    let result = child.wait_with_output()?;
    if !result.status.success() {
        bail!("{}", String::from_utf8_lossy(&result.stderr).trim());
    }
    Ok(())
}

/// Launch Services must wait until this process has saved its windows and quit.
pub fn restart(bundle: &Path) -> anyhow::Result<()> {
    restart_command(bundle, std::process::id(), Path::new("/usr/bin/open"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

fn restart_command(bundle: &Path, pid: u32, opener: &Path) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "i=0; while kill -0 \"$1\" 2>/dev/null; do i=$((i+1)); [ \"$i\" -lt 120 ] || exit 1; sleep 1; done; exec \"$3\" -n \"$2\"", "phux-restart"])
        .arg(pid.to_string()).arg(bundle).arg(opener);
    command
}

fn output(command: &mut Command) -> anyhow::Result<String> {
    let result = command
        .output()
        .context("Could not start updater command")?;
    decode_output(result)
}

fn decode_output(result: std::process::Output) -> anyhow::Result<String> {
    if !result.status.success() {
        bail!("{}", failure_message(&result.stderr));
    }
    String::from_utf8(result.stdout).context("Invalid updater output")
}

/// A read-only CLI check must not leave every update control busy forever.
/// Give it a private process group so its downloader is reaped on timeout too.
fn check_output(command: &mut Command) -> anyhow::Result<String> {
    check_output_until(command, std::time::Duration::from_secs(60))
}

fn check_output_until(
    command: &mut Command,
    timeout: std::time::Duration,
) -> anyhow::Result<String> {
    use std::os::unix::process::CommandExt;
    use std::time::Duration;
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .context("Could not start phux release check")?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(result) => decode_output(result?),
        Err(_) => {
            let _ = Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{pid}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = rx.recv_timeout(Duration::from_secs(2));
            bail!("phux release check timed out. Nothing was installed; try Check now again.")
        }
    }
}

fn failure_message(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return text.trim().to_owned();
    };
    let Some(message) = value["error"]["message"].as_str() else {
        return text.trim().to_owned();
    };
    format!("{message} {}", value["remedy"].as_str().unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_are_opt_in_and_latest_is_stable() {
        let prefs = Preferences::from_value(&json!({"desktop":"latest"}));
        assert!(prefs.automatic);
        assert!(!prefs.manage_runtime);
        assert_eq!(prefs.desktop, Channel::Stable);
    }
    #[test]
    fn releases_ignore_cli_drafts_and_prereleases() {
        let rows = json!([
            {"tag_name":"v99.0.0", "draft":false,"prerelease":false},
            {"tag_name":"desktop-v1.2.3", "draft":false,"prerelease":false},
            {"tag_name":"desktop-v9.0.0", "draft":true,"prerelease":false},
            {"tag_name":"desktop-v2.0.0-alpha.1", "draft":false,"prerelease":true}
        ]);
        assert_eq!(stable_releases(&rows).unwrap().len(), 1);
        assert!(stable_releases(&json!({})).is_err());
    }
    #[test]
    fn channel_switches_allow_return_from_next() {
        let stable = Release {
            version: "1.2.3".into(),
            revision: "a".repeat(40),
            channel: Channel::Stable,
        };
        let next = Release {
            version: "next".into(),
            revision: "b".repeat(40),
            channel: Channel::Next,
        };
        assert!(available(&next, &stable));
        assert!(available(&stable, &next));
        assert!(!available(&next, &next));
        assert!(!available(&stable, &stable));
        assert!(stable_version("desktop-v01.2.3").is_none());
        assert!(stable_version("desktop-v1.2.3-alpha.1").is_none());
        let older = Release {
            version: "1.2.2".into(),
            ..stable.clone()
        };
        assert!(!available(&stable, &older));
    }
    #[test]
    fn only_standard_bundle_is_replaceable() {
        assert_eq!(
            app_bundle(Path::new(
                "/Applications/Phux.app/Contents/MacOS/phux-desktop"
            )),
            Some(PathBuf::from("/Applications/Phux.app"))
        );
        assert!(app_bundle(Path::new("target/debug/phux-desktop")).is_none());
        assert!(
            app_bundle(Path::new(
                "/Applications/Other.app/Contents/MacOS/phux-desktop"
            ))
            .is_none()
        );
    }

    #[test]
    fn runtime_handoff_refusal_is_not_a_restart() {
        let refused =
            json!({"schema_version":1,"server_handoff":{"result":"refused","detail":"busy"}});
        let result = runtime_result(&refused).unwrap();
        assert!(!result.reconnect);
        assert!(result.retry_handoff);
        assert!(result.message.contains("busy"));
        assert!(runtime_result(&json!({"schema_version":2})).is_err());
        let accepted = json!({"schema_version":1,"server_handoff":{"result":"upgrading"}});
        assert!(runtime_result(&accepted).unwrap().reconnect);
    }

    #[test]
    fn next_requires_a_desktop_ready_pointer_not_the_cli_pointer() {
        let sha = "a".repeat(40);
        let pointer = json!({"schema_version":1,"product":"desktop","channel":"next","sha":sha,"version":"0.3.0","archive":format!("phux-desktop-next.{sha}-macos-arm64.zip")});
        assert_eq!(next_release(&pointer).unwrap().revision, sha);
        assert!(next_release(&json!({"sha":"a".repeat(40)})).is_err());
        let mut invalid = pointer.clone();
        invalid["sha"] = json!("../malicious");
        assert!(next_release(&invalid).is_err());
        invalid = pointer.clone();
        invalid["archive"] = json!("other.zip");
        assert!(next_release(&invalid).is_err());
        invalid = pointer;
        invalid["version"] = json!("bad");
        assert!(next_release(&invalid).is_err());
    }

    #[test]
    fn structured_failure_has_readable_remedy() {
        assert_eq!(
            failure_message(
                br#"{"error":{"message":"Cannot update"},"remedy":"Use brew upgrade phux"}"#
            ),
            "Cannot update Use brew upgrade phux"
        );
        assert_eq!(
            failure_message(b"network unavailable\n"),
            "network unavailable"
        );
    }

    #[test]
    fn restart_passes_bundle_as_data_without_shell_expansion() {
        let bundle = Path::new("/Applications/Phux.app; $(touch should-not-exist)");
        let result = restart_command(bundle, u32::MAX, Path::new("/bin/echo"))
            .output()
            .unwrap();
        assert!(result.status.success());
        assert_eq!(
            String::from_utf8(result.stdout).unwrap().trim(),
            bundle.to_str().unwrap()
        );
    }

    #[test]
    fn read_only_check_timeout_reaps_the_downloader() {
        let start = std::time::Instant::now();
        let result = check_output_until(
            Command::new("/bin/sh").args(["-c", "sleep 30"]),
            std::time::Duration::from_millis(30),
        );
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
    }

    #[cfg(unix)]
    #[test]
    fn runtime_commands_use_explicit_socket_and_never_close_agents() {
        use std::os::unix::fs::PermissionsExt;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("phux-update-test-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let phux = dir.join("phux");
        let record = dir.join("args");
        let document = json!({"schema_version":1,"current_version":"1.0.0","target_version":"next","update_available":true,"install":{"mutable":true},"server_handoff":{"result":"upgrading"}});
        std::fs::write(
            &phux,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat <<'JSON'\n{document}\nJSON\n",
                record.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&phux, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(runtime_check(&phux, Channel::Next).unwrap().available);
        assert_eq!(
            std::fs::read_to_string(&record).unwrap(),
            "update\n--check\n--channel\nnext\n--json\n"
        );
        assert!(
            runtime_install(&phux, &dir.join("isolated.sock"), Channel::Stable)
                .unwrap()
                .reconnect
        );
        let args = std::fs::read_to_string(&record).unwrap();
        assert!(args.starts_with(&format!(
            "--socket\n{}\nupdate\n",
            dir.join("isolated.sock").display()
        )));
        assert!(!args.contains("close-agent-sessions"));
        assert!(
            runtime_retry(&phux, &dir.join("isolated.sock"))
                .unwrap()
                .reconnect
        );
        assert_eq!(
            std::fs::read_to_string(&record).unwrap(),
            format!(
                "--socket\n{}\nupgrade\n",
                dir.join("isolated.sock").display()
            )
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
