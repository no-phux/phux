//! Release checks and explicit updates (ADR-0175, ADR-0176). A check reads one
//! channel manifest; an update stages a verified bundle beside the app while it
//! runs and a detached helper swaps it in only after this process has exited.
//! No downloaded code runs during a check.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, bail};
pub use phux_client_runtime::release::Channel;
use phux_client_runtime::release::{self, Build, MANIFEST_CURL_ARGS, Product, Version};
use serde_json::{Value, json};

/// A channel's name in the Updates page.
pub const fn channel_label(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => "Stable",
        Channel::Next => "Next",
    }
}

fn channel_of(value: &Value) -> Channel {
    value.as_str().and_then(Channel::parse).unwrap_or_default()
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
            desktop: channel_of(&value["desktop"]),
            runtime: channel_of(&value["runtime"]),
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
                "automatic": self.automatic, "desktop": self.desktop.as_str(),
                "runtime": self.runtime.as_str(), "manage_runtime": self.manage_runtime,
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

const TARGET: &str = "aarch64-apple-darwin";
/// Beside the installed app, so the post-exit swap is a same-volume rename.
const STAGING_DIR: &str = ".phux-desktop-update";
/// The installer's lock; every publisher of `Phux.app` takes the same inode.
const LOCK_FILE: &str = ".phux-desktop-install.lock";
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);
const STAGE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub revision: String,
    pub channel: Channel,
    /// The archive digest the manifest publishes; empty for the installed app.
    pub sha256: String,
    /// The wire `major.minor` this build links.
    pub protocol: String,
}

impl Release {
    /// The build this release is, or `None` for an unversioned bundle.
    fn build(&self) -> Option<Build> {
        match self.channel {
            Channel::Stable => Version::parse(&self.version).map(Build::Stable),
            Channel::Next => Some(Build::Next(self.revision.clone())),
        }
    }
}

/// The wire protocol this desktop links.
pub fn own_protocol() -> String {
    let version = phux_protocol::PROTOCOL_VERSION;
    format!("{}.{}", version.major, version.minor)
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
        sha256: String::new(),
        protocol: own_protocol(),
    })
}

fn not_published(channel: Channel) -> anyhow::Error {
    anyhow::anyhow!(
        "No Desktop {} build is published yet. Check again later.",
        channel_label(channel)
    )
}

pub fn latest(channel: Channel) -> anyhow::Result<Release> {
    let body = output(
        Command::new("/usr/bin/curl")
            .args(MANIFEST_CURL_ARGS)
            .arg(release::manifest_url(channel)),
    )
    .map_err(|error| {
        if error.to_string().contains("returned error: 404") {
            not_published(channel)
        } else {
            error
        }
    })?;
    manifest_release(&body, channel)
}

/// The desktop entry of a channel manifest.
pub fn manifest_release(body: &str, channel: Channel) -> anyhow::Result<Release> {
    let entry = release::parse(body, channel, Product::Desktop, TARGET)?
        .ok_or_else(|| not_published(channel))?;
    Ok(Release {
        version: entry.version.to_string(),
        revision: entry.sha,
        channel,
        sha256: entry.sha256,
        protocol: entry.protocol,
    })
}

/// Whether `target` updates `current`; an unversioned bundle always updates.
pub fn available(current: &Release, target: &Release) -> bool {
    let Some(target) = target.build() else {
        return false;
    };
    current
        .build()
        .is_none_or(|current| release::is_newer(&current, &target))
}

#[derive(Clone, Debug)]
pub struct RuntimeRelease {
    pub current: String,
    pub target: String,
    pub available: bool,
    pub mutable: bool,
    pub native_command: Option<String>,
    /// The installed phux's wire protocol; absent from older CLIs.
    pub protocol_current: Option<String>,
    /// The target release's wire protocol, when the CLI knows it.
    pub protocol_target: Option<String>,
}

pub fn runtime_check(phux: &Path, channel: Channel) -> anyhow::Result<RuntimeRelease> {
    let text = run_bounded(
        Command::new(phux).args(["update", "--check", "--channel", channel.as_str(), "--json"]),
        None,
        CHECK_TIMEOUT,
        "phux release check timed out. Nothing was installed; try Check now again.",
    )?;
    runtime_release(&serde_json::from_str(&text)?)
}

pub fn runtime_release(value: &Value) -> anyhow::Result<RuntimeRelease> {
    if value["schema_version"] != 1 && value["schema_version"] != 2 {
        bail!("Unsupported phux update response");
    }
    let text = |key: &str| value["protocol"][key].as_str().map(str::to_owned);
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
        protocol_current: text("current"),
        protocol_target: text("target"),
    })
}

pub struct RuntimeInstalled {
    pub message: String,
    pub reconnect: bool,
    pub retry_handoff: bool,
}

pub fn runtime_result(value: &Value) -> anyhow::Result<RuntimeInstalled> {
    if value["schema_version"] != 1 && value["schema_version"] != 2 {
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
    let text = run_bounded(
        Command::new(phux).arg("--socket").arg(socket).args([
            "update",
            "--channel",
            channel.as_str(),
            "--json",
        ]),
        None,
        STAGE_TIMEOUT,
        "phux update timed out. The previous binaries stay in place; try again.",
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

fn applications_dir(bundle: &Path) -> anyhow::Result<&Path> {
    bundle.parent().context("No Applications directory")
}

/// `<apps>/.phux-desktop-update`, which holds at most one staged `Phux.app`.
pub fn staging_dir(bundle: &Path) -> anyhow::Result<PathBuf> {
    Ok(applications_dir(bundle)?.join(STAGING_DIR))
}

fn lock_file(bundle: &Path) -> anyhow::Result<PathBuf> {
    Ok(applications_dir(bundle)?.join(LOCK_FILE))
}

fn stage_command(bundle: &Path, target: &Release) -> anyhow::Result<Command> {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-s", "--", "--desktop", "--stage-dir"])
        .arg(staging_dir(bundle)?)
        .args(["--channel", target.channel.as_str()]);
    match target.channel {
        Channel::Stable => command.args(["--version", &target.version]),
        Channel::Next => command.args(["--revision", &target.revision]),
    };
    command.args(["--sha256", &target.sha256]);
    Ok(command)
}

/// Download, verify and unpack `target` into the staging directory with the
/// shipped installer. Nothing outside the staging directory changes.
pub fn stage(bundle: &Path, target: &Release) -> anyhow::Result<PathBuf> {
    stage_with(bundle, target, |_| {})
}

/// [`stage`], with `configure` adjusting the installer's command (tests point
/// its downloads at a fixture).
fn stage_with(
    bundle: &Path,
    target: &Release,
    configure: impl FnOnce(&mut Command),
) -> anyhow::Result<PathBuf> {
    if !release::is_lower_hex(&target.sha256, 64) {
        bail!("No verified digest for the Desktop update");
    }
    let mut command = stage_command(bundle, target)?;
    configure(&mut command);
    let text = run_bounded(
        &mut command,
        Some(include_bytes!("../../../../scripts/install.sh")),
        STAGE_TIMEOUT,
        "Desktop update download timed out after 15 minutes. Nothing was installed.",
    )?;
    let staged = staged_path(&text, &staging_dir(bundle)?)?;
    let built = installed_release(&staged)?;
    let matches = match target.channel {
        Channel::Stable => built.version == target.version,
        Channel::Next => built.revision == target.revision,
    };
    if !matches || built.channel != target.channel {
        bail!("The staged Desktop bundle is not the release that was checked");
    }
    Ok(staged)
}

/// The installer's final `staged: <dir>/Phux.app` line, which must name our
/// staging directory.
fn staged_path(stdout: &str, staging: &Path) -> anyhow::Result<PathBuf> {
    let path = stdout
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("staged: "))
        .map(PathBuf::from)
        .context("The installer did not report a staged bundle")?;
    if path != staging.join("Phux.app") || !path.is_dir() {
        bail!(
            "The installer staged {} outside the update directory",
            path.display()
        );
    }
    Ok(path)
}

/// `from_bundle APP`: whether any process runs `APP`'s executable. It matches
/// the command's prefix, so the `ps`/`awk` pipeline (whose own arguments hold
/// no path) never matches itself; the path travels through the environment,
/// not awk's escape-processing `-v`.
macro_rules! from_bundle_fn {
    () => {
        r#"from_bundle() {
  /bin/ps -axww -o command= | PHUX_EXE="$1/Contents/MacOS/phux-desktop" /usr/bin/awk \
    'index($0, ENVIRON["PHUX_EXE"]) == 1 { found = 1 } END { exit !found }'
}
"#
    };
}

/// Waits for the app to exit, then takes the installer lock and runs SWAP.
/// Every path arrives as a positional parameter, never as script text.
const WAIT_THEN_SWAP: &str = concat!(
    from_bundle_fn!(),
    r#"pid=$1; lock=$2; swap=$7
echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) waiting for Phux ($pid) to exit"
# Any instance still running from the installed bundle keeps it in place.
running() {
  kill -0 "$pid" 2>/dev/null \
    || from_bundle "$4"
}
i=0
while running "$@"; do
  i=$((i + 1))
  if [ "$i" -ge 600 ]; then
    echo "Phux did not exit; the staged update stays in place"
    exit 1
  fi
  sleep 1
done
exec /usr/bin/lockf -s -t 30 "$lock" /bin/sh -c "$swap" phux-swap "$3" "$4" "$5" "$6""#
);

const SWAP: &str = concat!(
    from_bundle_fn!(),
    r#"staged=$1; installed=$2; relaunch=$3; opener=$4
staging=${staged%/*}
previous="$staging/Phux.previous.app"
if [ ! -d "$staged" ]; then
  echo "no staged update at $staged; nothing changed"
  exit 1
fi
# Rechecked under the lock: an instance opened while waiting keeps its bundle.
if from_bundle "$installed"; then
  echo "Phux is running again from $installed; the staged update stays for the next quit"
  exit 1
fi
rm -rf "$previous"
if [ -e "$installed" ] && ! mv "$installed" "$previous"; then
  echo "could not move the installed app aside; nothing changed"
  exit 1
fi
if ! mv "$staged" "$installed"; then
  [ ! -e "$previous" ] || mv "$previous" "$installed"
  echo "could not install the staged update; the previous app is restored"
  exit 1
fi
rm -rf "$staging"
/usr/bin/xattr -d com.apple.quarantine "$installed" 2>/dev/null || true
echo "installed the update at $installed"
if [ "$relaunch" = 1 ]; then
  exec "$opener" "$installed"
fi"#
);

fn apply_command(
    bundle: &Path,
    staged: &Path,
    pid: u32,
    relaunch: bool,
    opener: &Path,
) -> anyhow::Result<Command> {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", WAIT_THEN_SWAP, "phux-update", &pid.to_string()])
        .arg(lock_file(bundle)?)
        .arg(staged)
        .arg(bundle)
        .arg(if relaunch { "1" } else { "0" })
        .arg(opener)
        .arg(SWAP);
    Ok(command)
}

fn update_log() -> Stdio {
    let Some(home) = std::env::var_os("HOME") else {
        return Stdio::null();
    };
    let dir = PathBuf::from(home).join("Library/Logs/Phux");
    std::fs::create_dir_all(&dir)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("update.log"))
        })
        .map_or_else(|_| Stdio::null(), Stdio::from)
}

/// Hand the staged bundle to a detached helper that installs it after this
/// process exits, and relaunches it when asked. The running bundle is never
/// touched while this process uses it.
pub fn apply_on_exit(bundle: &Path, staged: &Path, relaunch: bool) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut command = apply_command(
        bundle,
        staged,
        std::process::id(),
        relaunch,
        Path::new("/usr/bin/open"),
    )?;
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(update_log())
        .stderr(update_log())
        .spawn()
        .context("Could not start the update helper")?;
    Ok(())
}

/// A staged bundle from an earlier session is discarded at launch, but only
/// when no helper holds the installer lock.
pub fn clear_stale_stage(bundle: &Path) {
    let (Ok(dir), Ok(lock)) = (staging_dir(bundle), lock_file(bundle)) else {
        return;
    };
    if !dir.exists() {
        return;
    }
    let _ = Command::new("/usr/bin/lockf")
        .args(["-s", "-t", "0"])
        .arg(lock)
        .arg("/bin/rm")
        .arg("-rf")
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
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

/// Run `command` in a private process group, feeding `input` on stdin, and kill
/// the whole group (its downloaders included) when `timeout` elapses, so no
/// update control stays busy forever.
fn run_bounded(
    command: &mut Command,
    input: Option<&'static [u8]>,
    timeout: Duration,
    timed_out: &str,
) -> anyhow::Result<String> {
    use std::os::unix::process::CommandExt;
    command
        .process_group(0)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().context("Could not start updater command")?;
    let pid = child.id();
    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        // A separate writer: the child may fill stdout before it reads all of stdin.
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(bytes);
        });
    }
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
            bail!("{timed_out}")
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

    const STABLE: &str = include_str!("../../../../scripts/fixtures/release-manifest/stable.json");
    const NEXT: &str = include_str!("../../../../scripts/fixtures/release-manifest/next.json");

    fn scratch(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "phux-update-{name}-{}-{nonce} $(touch pwned)",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn preferences_are_opt_in_and_latest_is_stable() {
        let prefs = Preferences::from_value(&json!({"desktop":"latest"}));
        assert!(prefs.automatic);
        assert!(!prefs.manage_runtime);
        assert_eq!(prefs.desktop, Channel::Stable);
    }

    #[test]
    fn golden_manifests_name_the_desktop_release() {
        let stable = manifest_release(STABLE, Channel::Stable).unwrap();
        assert_eq!(stable.version, "1.2.3");
        assert_eq!(stable.revision, "b".repeat(40));
        assert_eq!(stable.sha256, "d".repeat(64));
        assert_eq!(stable.protocol, "0.9");
        let next = manifest_release(NEXT, Channel::Next).unwrap();
        assert_eq!(next.channel, Channel::Next);
        assert!(manifest_release(STABLE, Channel::Next).is_err());
    }

    #[test]
    fn a_channel_without_desktop_reads_as_unpublished() {
        let mut document: Value = serde_json::from_str(NEXT).unwrap();
        document["products"]
            .as_object_mut()
            .unwrap()
            .remove("desktop");
        let error = manifest_release(&document.to_string(), Channel::Next).unwrap_err();
        assert_eq!(
            error.to_string(),
            "No Desktop Next build is published yet. Check again later."
        );
    }

    #[test]
    fn channel_switches_allow_return_from_next() {
        let stable = Release {
            version: "1.2.3".into(),
            revision: "a".repeat(40),
            ..Release::default()
        };
        let next = Release {
            revision: "b".repeat(40),
            channel: Channel::Next,
            ..stable.clone()
        };
        assert!(available(&next, &stable));
        assert!(available(&stable, &next));
        assert!(!available(&next, &next));
        assert!(!available(&stable, &stable));
        let older = Release {
            version: "1.2.2".into(),
            ..stable.clone()
        };
        assert!(!available(&stable, &older));
        assert!(available(&older, &stable));
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
    fn stage_passes_the_checked_release_and_its_digest() {
        let bundle = Path::new("/Applications/Phux.app");
        let stable = Release {
            version: "1.2.3".into(),
            sha256: "d".repeat(64),
            ..Release::default()
        };
        let args = |release: &Release| -> Vec<String> {
            stage_command(bundle, release)
                .unwrap()
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        let digest = "d".repeat(64);
        assert_eq!(
            args(&stable),
            [
                "-s",
                "--",
                "--desktop",
                "--stage-dir",
                "/Applications/.phux-desktop-update",
                "--channel",
                "stable",
                "--version",
                "1.2.3",
                "--sha256",
                &digest,
            ]
        );
        let next = Release {
            channel: Channel::Next,
            revision: "b".repeat(40),
            ..stable
        };
        assert!(
            args(&next)
                .windows(2)
                .any(|pair| pair == ["--revision", &"b".repeat(40)])
        );
        assert!(stage(bundle, &Release::default()).is_err());
    }

    /// Network: stages a published release into a scratch "Applications".
    /// `PHUX_E2E_DESKTOP=<version>:<sha256> cargo test -- --ignored stages_a_published`
    #[test]
    #[ignore = "downloads a published release"]
    fn stages_a_published_release() {
        let spec = std::env::var("PHUX_E2E_DESKTOP").unwrap();
        let (version, sha256) = spec.split_once(':').unwrap();
        let apps = scratch("e2e");
        let target = Release {
            version: version.into(),
            sha256: sha256.into(),
            ..Release::default()
        };
        let staged = stage(&apps.join("Phux.app"), &target).unwrap();
        assert_eq!(staged, apps.join(".phux-desktop-update/Phux.app"));
        assert_eq!(installed_release(&staged).unwrap().version, version);
        let mut wrong = target.clone();
        wrong.sha256 = "0".repeat(64);
        assert!(stage(&apps.join("Phux.app"), &wrong).is_err());
        assert!(!apps.join("Phux.app").exists(), "staging never installs");
        std::fs::remove_dir_all(apps).unwrap();
    }

    #[test]
    fn the_staged_path_must_be_our_staging_bundle() {
        let dir = scratch("staged-path");
        let staged = dir.join("Phux.app");
        std::fs::create_dir(&staged).unwrap();
        let line = format!("verified\nstaged: {}\n", staged.display());
        assert_eq!(staged_path(&line, &dir).unwrap(), staged);
        assert!(staged_path("staged: /tmp/elsewhere/Phux.app\n", &dir).is_err());
        assert!(staged_path("no marker\n", &dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn fake_app(path: &Path, marker: &str) {
        std::fs::create_dir_all(path.join("Contents")).unwrap();
        std::fs::write(path.join("Contents/marker"), marker).unwrap();
    }

    fn run_swap(apps: &Path, relaunch: bool) -> (std::process::Output, PathBuf) {
        let opened = apps.join("opened");
        let opener = apps.join("open");
        std::fs::write(
            &opener,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                opened.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&opener, std::fs::Permissions::from_mode(0o700)).unwrap();
        let bundle = apps.join("Phux.app");
        let staged = staging_dir(&bundle).unwrap().join("Phux.app");
        // A process that has already exited stands in for the quitting app.
        let mut exited = Command::new("/usr/bin/true").spawn().unwrap();
        let pid = exited.id();
        exited.wait().unwrap();
        let result = apply_command(&bundle, &staged, pid, relaunch, &opener)
            .unwrap()
            .output()
            .unwrap();
        (result, opened)
    }

    #[test]
    fn the_helper_swaps_after_exit_and_relaunches() {
        let apps = scratch("swap");
        let bundle = apps.join("Phux.app");
        fake_app(&bundle, "old");
        fake_app(&staging_dir(&bundle).unwrap().join("Phux.app"), "new");
        let (result, opened) = run_swap(&apps, true);
        assert!(result.status.success(), "{result:?}");
        assert_eq!(
            std::fs::read_to_string(bundle.join("Contents/marker")).unwrap(),
            "new"
        );
        assert!(!staging_dir(&bundle).unwrap().exists());
        assert_eq!(
            std::fs::read_to_string(&opened).unwrap(),
            format!("{}\n", bundle.display())
        );
        assert!(!apps.join("pwned").exists() && !Path::new("pwned").exists());
        std::fs::remove_dir_all(apps).unwrap();
    }

    /// Another instance running from the installed bundle keeps it in place:
    /// nothing is moved or deleted from under a live process.
    #[test]
    fn a_running_instance_keeps_its_bundle() {
        let apps = scratch("running");
        let bundle = apps.join("Phux.app");
        fake_app(&bundle, "old");
        let staged = staging_dir(&bundle).unwrap().join("Phux.app");
        fake_app(&staged, "new");
        let executable = bundle.join("Contents/MacOS/phux-desktop");
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::copy("/bin/sleep", &executable).unwrap();
        // A moved platform binary is killed on launch unless re-signed.
        let signed = Command::new("/usr/bin/codesign")
            .args(["-s", "-", "-f"])
            .arg(&executable)
            .output()
            .unwrap();
        assert!(signed.status.success(), "{signed:?}");
        let mut instance = Command::new(&executable).arg("30").spawn().unwrap();
        let result = Command::new("/bin/sh")
            .args(["-c", SWAP, "phux-swap"])
            .arg(&staged)
            .arg(&bundle)
            .args(["0", "/usr/bin/true"])
            .output()
            .unwrap();
        instance.kill().unwrap();
        instance.wait().unwrap();
        assert!(!result.status.success(), "{result:?}");
        assert_eq!(
            std::fs::read_to_string(bundle.join("Contents/marker")).unwrap(),
            "old"
        );
        assert!(staged.is_dir(), "the staged update stays for the next quit");
        std::fs::remove_dir_all(apps).unwrap();
    }

    #[test]
    fn a_plain_quit_installs_without_relaunching() {
        let apps = scratch("quit");
        let bundle = apps.join("Phux.app");
        fake_app(&bundle, "old");
        fake_app(&staging_dir(&bundle).unwrap().join("Phux.app"), "new");
        let (result, opened) = run_swap(&apps, false);
        assert!(result.status.success(), "{result:?}");
        assert!(!opened.exists());
        std::fs::remove_dir_all(apps).unwrap();
    }

    #[test]
    fn a_missing_staged_bundle_leaves_the_installed_app_alone() {
        let apps = scratch("missing");
        let bundle = apps.join("Phux.app");
        fake_app(&bundle, "old");
        let (result, opened) = run_swap(&apps, true);
        assert!(!result.status.success());
        assert_eq!(
            std::fs::read_to_string(bundle.join("Contents/marker")).unwrap(),
            "old"
        );
        assert!(!opened.exists());
        std::fs::remove_dir_all(apps).unwrap();
    }

    /// A signed `Phux.app` release archive served to the shipped installer by
    /// a fake `curl`, beside an installed app that every refusal must leave
    /// alone.
    struct Network {
        root: PathBuf,
        bundle: PathBuf,
    }

    impl Network {
        fn new(name: &str) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let root = scratch(name);
            let bin = root.join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            // Serves the archive whatever the URL, then exits with `status`.
            std::fs::write(
                bin.join("curl"),
                "#!/bin/sh\n\
                 while [ \"$#\" -gt 0 ]; do case \"$1\" in -o) out=\"$2\"; shift ;; esac; shift; done\n\
                 cp \"$FIXTURE/asset.zip\" \"$out\"\n\
                 exit \"$(cat \"$FIXTURE/status\" 2>/dev/null || echo 0)\"\n",
            )
            .unwrap();
            std::fs::set_permissions(bin.join("curl"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            let bundle = root.join("Applications/Phux.app");
            fake_app(&bundle, "old");
            Self { root, bundle }
        }

        /// Pack a signed bundle identifying as `identifier`, run `tamper` on
        /// it after signing, and return the archive's digest.
        fn publish(&self, identifier: &str, tamper: impl FnOnce(&Path)) -> String {
            let app = self.root.join("source/Phux.app");
            std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
            std::fs::copy("/usr/bin/true", app.join("Contents/MacOS/phux-desktop")).unwrap();
            std::fs::write(
                app.join("Contents/Info.plist"),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{identifier}</string>
<key>CFBundleExecutable</key><string>phux-desktop</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>PhuxDesktopVersion</key><string>1.2.3</string>
<key>PhuxDesktopChannel</key><string>stable</string>
<key>PhuxBuildSHA</key><string>{sha}</string>
</dict></plist>
"#,
                    sha = "b".repeat(40)
                ),
            )
            .unwrap();
            let signed = Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-"])
                .arg(&app)
                .output()
                .unwrap();
            assert!(signed.status.success(), "{signed:?}");
            tamper(&app);
            let archive = self.root.join("asset.zip");
            let packed = Command::new("/usr/bin/ditto")
                .args(["-c", "-k", "--keepParent", "--norsrc"])
                .arg(&app)
                .arg(&archive)
                .output()
                .unwrap();
            assert!(packed.status.success(), "{packed:?}");
            let digest = output(
                Command::new("/usr/bin/shasum")
                    .args(["-a", "256"])
                    .arg(&archive),
            )
            .unwrap();
            digest.split_whitespace().next().unwrap().to_owned()
        }

        /// The stable channel manifest publishing `digest` for the archive,
        /// read the way the Updates page reads it, as an update to 1.2.2.
        fn checked(&self, digest: &str) -> Release {
            let mut document: Value = serde_json::from_str(STABLE).unwrap();
            document["products"]["desktop"]["artifacts"][TARGET]["sha256"] = json!(digest);
            let target = manifest_release(&document.to_string(), Channel::Stable).unwrap();
            let installed = Release {
                version: "1.2.2".into(),
                ..Release::default()
            };
            assert!(available(&installed, &target));
            target
        }

        fn stage(&self, target: &Release) -> anyhow::Result<PathBuf> {
            stage_with(&self.bundle, target, |command| {
                let bin = self.root.join("bin");
                command.env("FIXTURE", &self.root).env(
                    "PATH",
                    format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
                );
            })
        }

        /// Staging `target` fails with `message`, leaves the installed app
        /// untouched and stages nothing.
        fn assert_refused(self, target: &Release, message: &str) {
            let error = format!("{:#}", self.stage(target).unwrap_err());
            assert!(error.contains(message), "{error}");
            assert_eq!(
                std::fs::read_to_string(self.bundle.join("Contents/marker")).unwrap(),
                "old"
            );
            let staged = staging_dir(&self.bundle).unwrap().join("Phux.app");
            assert!(!staged.exists());
            std::fs::remove_dir_all(self.root).unwrap();
        }
    }

    #[test]
    fn a_checked_release_stages_beside_the_installed_app() {
        let network = Network::new("stage-ok");
        let target = network.checked(&network.publish("dev.phux.desktop", |_| {}));
        let staged = network.stage(&target).unwrap();
        assert_eq!(
            staged,
            staging_dir(&network.bundle).unwrap().join("Phux.app")
        );
        assert_eq!(installed_release(&staged).unwrap().version, "1.2.3");
        assert_eq!(
            std::fs::read_to_string(network.bundle.join("Contents/marker")).unwrap(),
            "old"
        );
        std::fs::remove_dir_all(network.root).unwrap();
    }

    #[test]
    fn a_digest_mismatch_is_refused() {
        let network = Network::new("stage-digest");
        network.publish("dev.phux.desktop", |_| {});
        let target = network.checked(&"0".repeat(64));
        network.assert_refused(&target, "checksum mismatch");
    }

    fn truncate_archive(network: &Network) {
        let archive = std::fs::read(network.root.join("asset.zip")).unwrap();
        std::fs::write(
            network.root.join("asset.zip"),
            &archive[..archive.len() / 2],
        )
        .unwrap();
    }

    #[test]
    fn an_interrupted_download_is_refused() {
        let network = Network::new("stage-interrupted");
        let target = network.checked(&network.publish("dev.phux.desktop", |_| {}));
        truncate_archive(&network);
        // curl's "partial file": the transfer stopped before the end.
        std::fs::write(network.root.join("status"), "18").unwrap();
        network.assert_refused(&target, "could not download");
    }

    #[test]
    fn a_truncated_download_is_refused() {
        let network = Network::new("stage-truncated");
        let target = network.checked(&network.publish("dev.phux.desktop", |_| {}));
        truncate_archive(&network);
        network.assert_refused(&target, "checksum mismatch");
    }

    #[test]
    fn an_archive_for_another_bundle_identity_is_refused() {
        let network = Network::new("stage-identity");
        let target = network.checked(&network.publish("dev.example.other", |_| {}));
        network.assert_refused(&target, "not Phux Desktop");
    }

    #[test]
    fn a_tampered_bundle_is_refused_even_when_the_manifest_matches_it() {
        let network = Network::new("stage-tampered");
        let target = network.checked(&network.publish("dev.phux.desktop", |app| {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .append(true)
                .open(app.join("Contents/MacOS/phux-desktop"))
                .unwrap()
                .write_all(b"tampered")
                .unwrap();
        }));
        network.assert_refused(&target, "signature does not verify");
    }

    #[test]
    fn a_stale_stage_is_cleared_only_when_the_lock_is_free() {
        let apps = scratch("stale");
        let bundle = apps.join("Phux.app");
        let staged = staging_dir(&bundle).unwrap().join("Phux.app");
        fake_app(&staged, "stale");
        let lock = lock_file(&bundle).unwrap();
        let mut holder = Command::new("/usr/bin/lockf")
            .arg(&lock)
            .args(["/bin/sleep", "5"])
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        while !lock.exists() && start.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(100));
        clear_stale_stage(&bundle);
        assert!(
            staged.exists(),
            "a held lock must protect the staged bundle"
        );
        holder.kill().unwrap();
        holder.wait().unwrap();
        clear_stale_stage(&bundle);
        assert!(!staging_dir(&bundle).unwrap().exists());
        std::fs::remove_dir_all(apps).unwrap();
    }

    #[test]
    fn runtime_handoff_refusal_is_not_a_restart() {
        let refused =
            json!({"schema_version":2,"server_handoff":{"result":"refused","detail":"busy"}});
        let result = runtime_result(&refused).unwrap();
        assert!(!result.reconnect);
        assert!(result.retry_handoff);
        assert!(result.message.contains("busy"));
        assert!(runtime_result(&json!({"schema_version":3})).is_err());
        let accepted = json!({"schema_version":1,"server_handoff":{"result":"upgrading"}});
        assert!(runtime_result(&accepted).unwrap().reconnect);
    }

    #[test]
    fn runtime_protocol_is_optional() {
        let base = json!({"schema_version":2,"current_version":"1.0.0","target_version":"v1.1.0",
            "update_available":true,"install":{"mutable":true}});
        let old = runtime_release(&base).unwrap();
        assert_eq!(old.protocol_current, None);
        let mut new = base;
        new["protocol"] = json!({"current":"0.9","target":"0.10"});
        let release = runtime_release(&new).unwrap();
        assert_eq!(release.protocol_current.as_deref(), Some("0.9"));
        assert_eq!(release.protocol_target.as_deref(), Some("0.10"));
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
    fn a_timed_out_command_has_its_process_group_killed() {
        let start = std::time::Instant::now();
        let result = run_bounded(
            Command::new("/bin/sh").args(["-c", "sleep 30 & wait"]),
            None,
            Duration::from_millis(30),
            "timed out",
        );
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn bounded_runs_feed_stdin() {
        let text = run_bounded(
            Command::new("/bin/sh").arg("-s"),
            Some(b"echo fed\n"),
            Duration::from_secs(10),
            "timed out",
        )
        .unwrap();
        assert_eq!(text, "fed\n");
    }

    #[cfg(unix)]
    #[test]
    fn runtime_commands_use_explicit_socket_and_never_close_agents() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("runtime");
        let phux = dir.join("phux");
        let record = dir.join("args");
        let document = json!({"schema_version":2,"current_version":"1.0.0","target_version":"next","update_available":true,"install":{"mutable":true},"server_handoff":{"result":"upgrading"}});
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
