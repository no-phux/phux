//! Release checks and explicit updates (ADR-0175, ADR-0176). A check reads one
//! channel manifest; an update stages a verified bundle beside the app while it
//! runs and a detached helper swaps it in only after this process has exited.
//! No downloaded code runs during a check. Before staging, the manifest's
//! digest must equal the one a release key signed (ADR-0178); the installer
//! then holds the archive to that digest.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, bail};
pub use phux_client_runtime::release::Channel;
use phux_client_runtime::release::signature::{self, RELEASE_KEYS};
use phux_client_runtime::release::{self, Build, MANIFEST_CURL_ARGS, Product, Version};
use serde_json::{Value, json};

mod install;
mod progress;
mod runtime;

pub use install::*;
pub use runtime::*;

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
            manage_runtime: true,
        }
    }
}

impl Preferences {
    pub fn from_value(value: &Value) -> Self {
        Self {
            automatic: value["automatic"].as_bool().unwrap_or(true),
            desktop: channel_of(&value["desktop"]),
            runtime: channel_of(&value["runtime"]),
            manage_runtime: value["manage_runtime"].as_bool().unwrap_or(true),
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
    config_dir().join("updates.json")
}

/// The desktop's app-wide configuration directory: `phux-desktop` under
/// `LOCALAPPDATA` / `APPDATA` on Windows; `XDG_CONFIG_HOME`, else `~/.config`, on Unix.
pub(crate) fn config_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("AppData/Local")
        });
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("phux-desktop")
}

#[cfg(target_os = "macos")]
const TARGET: &str = "aarch64-apple-darwin";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const TARGET: &str = "x86_64-unknown-linux-gnu";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const TARGET: &str = "aarch64-unknown-linux-gnu";
#[cfg(windows)]
const TARGET: &str = WINDOWS_TARGET;
const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";
#[cfg(windows)]
const CURL: &str = "curl.exe";
#[cfg(not(windows))]
const CURL: &str = "/usr/bin/curl";
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
    match std::env::consts::OS {
        "linux" => linux_bundle(executable),
        "windows" => windows_bundle(executable),
        _ => macos_bundle(executable),
    }
}

fn macos_bundle(executable: &Path) -> Option<PathBuf> {
    if !executable.ends_with("Phux.app/Contents/MacOS/phux-desktop") {
        return None;
    }
    Some(executable.parent()?.parent()?.parent()?.to_path_buf())
}

fn linux_bundle(executable: &Path) -> Option<PathBuf> {
    if !executable.ends_with("phux-desktop/bin/phux-desktop") {
        return None;
    }
    let bundle = executable.parent()?.parent()?;
    linux_release(bundle).ok()?;
    Some(bundle.to_path_buf())
}

fn linux_release(bundle: &Path) -> anyhow::Result<Release> {
    portable_release(&bundle.join("share/phux-desktop/release.json"), None)
}

fn windows_bundle(executable: &Path) -> Option<PathBuf> {
    let standard = executable.ends_with("phux-desktop/phux-desktop.exe")
        || executable.ends_with("current/phux-desktop.exe");
    if !standard {
        return None;
    }
    let bundle = executable.parent()?;
    windows_release(bundle).ok()?;
    Some(bundle.to_path_buf())
}

fn windows_release(bundle: &Path) -> anyhow::Result<Release> {
    portable_release(
        &bundle.join("share/phux-desktop/release.json"),
        Some(WINDOWS_TARGET),
    )
}

fn portable_release(metadata: &Path, target: Option<&str>) -> anyhow::Result<Release> {
    let value: Value = serde_json::from_slice(&std::fs::read(metadata)?)?;
    if value["identifier"] != "dev.phux.desktop" {
        bail!("Not a Phux Desktop bundle");
    }
    if target.is_some_and(|target| value["target"] != target) {
        bail!("The Desktop bundle is not built for this target");
    }
    let version = value["version"]
        .as_str()
        .context("Missing desktop version")?;
    Version::parse(version).context("Invalid desktop version")?;
    let revision = value["sha"].as_str().context("Missing desktop revision")?;
    if !release::is_lower_hex(revision, 40) {
        bail!("Invalid desktop revision");
    }
    let channel = value["channel"]
        .as_str()
        .and_then(Channel::parse)
        .context("Invalid desktop channel")?;
    Ok(Release {
        version: version.into(),
        revision: revision.into(),
        channel,
        sha256: String::new(),
        protocol: own_protocol(),
    })
}

pub fn installed_release(bundle: &Path) -> anyhow::Result<Release> {
    if cfg!(target_os = "linux") {
        return linux_release(bundle);
    }
    if cfg!(windows) {
        return windows_release(bundle);
    }
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

/// Native updater utilities must not open console windows over the desktop UI.
fn native_command(program: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new(program);
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        command
    }
    #[cfg(not(windows))]
    {
        Command::new(program)
    }
}

pub fn latest(channel: Channel) -> anyhow::Result<Release> {
    let body = output(
        native_command(CURL)
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

/// Run `command`, feeding `input` on stdin, and kill its private Unix process
/// group or recorded Windows process tree on timeout. Never cancel by name.
pub(crate) fn run_bounded(
    command: &mut Command,
    input: Option<&'static [u8]>,
    timeout: Duration,
    timed_out: &str,
) -> anyhow::Result<String> {
    run_bounded_with_progress(command, input, timeout, timed_out, None, None, false)
}

/// Like [`run_bounded`], but the child calls `setsid` and captured stdout stops
/// at `max_stdout`. A timeout still kills that session's process group.
pub(crate) fn run_bounded_session(
    command: &mut Command,
    timeout: Duration,
    timed_out: &str,
    max_stdout: usize,
) -> anyhow::Result<String> {
    run_bounded_with_progress(
        command,
        None,
        timeout,
        timed_out,
        None,
        Some(max_stdout),
        true,
    )
}

fn run_bounded_with_progress(
    command: &mut Command,
    input: Option<&'static [u8]>,
    timeout: Duration,
    timed_out: &str,
    progress: Option<std::sync::mpsc::Sender<StageProgress>>,
    max_stdout: Option<usize>,
    new_session: bool,
) -> anyhow::Result<String> {
    #[cfg(unix)]
    if new_session {
        detach_session(command);
    } else {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
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
    // Drain stderr while the command runs: curl redraws its meter with CR,
    // not newline, and progress must not wait for the installer to exit.
    let stderr = child.stderr.take().context("No updater stderr")?;
    let stderr = std::thread::spawn(move || progress::read_stderr(stderr, progress));
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = collect_output(&mut child, stderr, max_stdout);
        // Retain the Windows process handle through cancellation or receipt.
        // An exited child's PID must not be reused before taskkill sees it.
        let _ = tx.send((result, child));
    });
    match rx.recv_timeout(timeout) {
        Ok((result, _child)) => decode_output(result?),
        Err(_) => {
            cancel_process_tree(pid);
            let _ = rx.recv_timeout(Duration::from_secs(2));
            bail!("{timed_out}")
        }
    }
}

/// `setsid` in the forked child, before exec, so the probe shell is a session
/// and process-group leader. Do not also `setpgid`: a session leader cannot
/// change its process group, and spawn would fail.
#[cfg(unix)]
#[allow(unsafe_code)]
fn detach_session(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure runs only in the forked child, before any non-safe
    // allocation and before exec. `setsid` is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            nix::unistd::setsid()
                .map(|_| ())
                .map_err(std::io::Error::from)
        });
    }
}

#[cfg(unix)]
fn cancel_process_tree(pid: u32) {
    let _ = Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(windows)]
fn cancel_process_tree(pid: u32) {
    // The PID belongs to the child we spawned, never a process-name/port match.
    // Bound taskkill too: cancellation must not leave the Updates UI waiting.
    let Ok(mut killer) = native_command("taskkill.exe")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while matches!(killer.try_wait(), Ok(None)) {
        if std::time::Instant::now() >= deadline {
            let _ = killer.kill();
            let _ = killer.wait();
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn collect_output(
    child: &mut std::process::Child,
    stderr: std::thread::JoinHandle<std::io::Result<Vec<u8>>>,
    max_stdout: Option<usize>,
) -> std::io::Result<std::process::Output> {
    use std::io::Read;
    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        if let Some(max) = max_stdout {
            // Keep the cap, but keep reading so a noisy shell does not block
            // on a full pipe or die from SIGPIPE before it exits.
            let mut buf = [0u8; 8192];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(read) => {
                        if stdout.len() < max {
                            let room = max - stdout.len();
                            stdout.extend_from_slice(&buf[..read.min(room)]);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(err) => return Err(err),
                }
            }
        } else {
            pipe.read_to_end(&mut stdout)?;
        }
    }
    let stderr = stderr
        .join()
        .map_err(|_| std::io::Error::other("Updater stderr reader panicked"))??;
    Ok(std::process::Output {
        status: child.wait()?,
        stdout,
        stderr,
    })
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
mod tests;
