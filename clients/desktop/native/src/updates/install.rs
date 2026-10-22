//! Staging a verified desktop bundle beside the app and the detached
//! helper that swaps it in after this process exits.

use super::*;

/// Archive transfer progress is not verification or staging completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageProgress {
    Downloading(u8),
    Verifying,
}

/// Beside the installed app, so the post-exit swap is a same-volume rename.
const STAGING_DIR: &str = ".phux-desktop-update";
/// The installer's lock; every publisher of `Phux.app` takes the same inode.
const LOCK_FILE: &str = ".phux-desktop-install.lock";

pub(super) fn applications_dir(bundle: &Path) -> anyhow::Result<&Path> {
    bundle.parent().context("No Applications directory")
}

/// `<apps>/.phux-desktop-update`, which holds at most one staged `Phux.app`.
pub fn staging_dir(bundle: &Path) -> anyhow::Result<PathBuf> {
    Ok(applications_dir(bundle)?.join(STAGING_DIR))
}

pub(super) fn lock_file(bundle: &Path) -> anyhow::Result<PathBuf> {
    Ok(applications_dir(bundle)?.join(LOCK_FILE))
}

pub(super) fn stage_command(bundle: &Path, target: &Release) -> anyhow::Result<Command> {
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
pub fn stage(
    bundle: &Path,
    target: &Release,
    progress: std::sync::mpsc::Sender<StageProgress>,
) -> anyhow::Result<PathBuf> {
    if cfg!(target_os = "linux") {
        bail!("{}", linux_install_remedy(target.channel, Some(bundle)));
    }
    stage_with_progress(
        bundle,
        target,
        |target| check_signed(target, RELEASE_KEYS, fetch),
        |_| {},
        Some(progress),
    )
}

pub fn linux_install_remedy(channel: Channel, bundle: Option<&Path>) -> String {
    let destination = bundle
        .and_then(Path::parent)
        .map_or_else(String::new, |directory| {
            format!(
                " --applications-dir {}",
                crate::attach::quote_paths(&[directory.to_path_buf()])
            )
        });
    format!(
        "Automatic Desktop installation is not supported on Linux. With minisign on PATH, quit Phux and run: \
         curl -fsSL https://phux.sh/install | PHUX_REQUIRE_SIGNATURE=1 sh -s -- \
         --desktop --channel {}{destination}",
        channel.as_str(),
    )
}

/// One small release file over HTTPS, bounded like a manifest fetch.
pub(super) fn fetch(url: &str) -> anyhow::Result<Vec<u8>> {
    let result = Command::new("/usr/bin/curl")
        .args(MANIFEST_CURL_ARGS)
        .arg(url)
        .output()
        .context("Could not start updater command")?;
    if !result.status.success() {
        bail!("{}", failure_message(&result.stderr));
    }
    Ok(result.stdout)
}

/// Require that a release key signed exactly the digest the manifest
/// publishes for `target` (ADR-0178). The manifest is unsigned, so its digest
/// alone would let a host that rewrites release assets vouch for any archive.
/// A missing or foreign signature refuses: an unsigned release is never
/// staged.
pub(super) fn check_signed(
    target: &Release,
    keys: &[&str],
    fetch: impl Fn(&str) -> anyhow::Result<Vec<u8>>,
) -> anyhow::Result<()> {
    let build = target
        .build()
        .context("No release version to verify for the Desktop update")?;
    let asset = release::asset(Product::Desktop, &build, TARGET);
    let sidecar = fetch(&asset.sidecar_url())
        .with_context(|| format!("Could not download the checksum for {}", asset.name))?;
    let minisig = fetch(&asset.signature_url()).with_context(|| {
        format!(
            "No signature for {} could be downloaded; an unsigned release is never installed",
            asset.name
        )
    })?;
    let signed = signature::signed_digest(&sidecar, &minisig, &asset.name, keys)?;
    if signed != target.sha256 {
        bail!(
            "The {} manifest publishes {} for {}, but the signed checksum is {signed}. \
             Nothing was installed.",
            target.channel.as_str(),
            target.sha256,
            asset.name
        );
    }
    Ok(())
}

/// [`stage`], with `signed` deciding whether the target's digest is trusted
/// and `configure` adjusting the installer's command (tests point its
/// downloads at a fixture).
#[cfg(all(test, target_os = "macos"))]
pub(super) fn stage_with(
    bundle: &Path,
    target: &Release,
    signed: impl FnOnce(&Release) -> anyhow::Result<()>,
    configure: impl FnOnce(&mut Command),
) -> anyhow::Result<PathBuf> {
    stage_with_progress(bundle, target, signed, configure, None)
}

pub(super) fn stage_with_progress(
    bundle: &Path,
    target: &Release,
    signed: impl FnOnce(&Release) -> anyhow::Result<()>,
    configure: impl FnOnce(&mut Command),
    progress: Option<std::sync::mpsc::Sender<StageProgress>>,
) -> anyhow::Result<PathBuf> {
    if !release::is_lower_hex(&target.sha256, 64) {
        bail!("No verified digest for the Desktop update");
    }
    signed(target)?;
    let mut command = stage_command(bundle, target)?;
    command.env(
        "PHUX_DESKTOP_PROGRESS",
        if progress.is_some() { "1" } else { "0" },
    );
    configure(&mut command);
    let text = run_bounded_with_progress(
        &mut command,
        Some(include_bytes!("../../../../../scripts/install.sh")),
        STAGE_TIMEOUT,
        "Desktop update download timed out after 15 minutes. Nothing was installed.",
        progress,
    )?;
    let staged = staged_path(&text, &staging_dir(bundle)?)?;
    verify_staged_release(&staged, target)?;
    Ok(staged)
}

pub(super) fn verify_staged_release(staged: &Path, target: &Release) -> anyhow::Result<()> {
    let built = installed_release(staged)?;
    let matches = match target.channel {
        Channel::Stable => built.version == target.version,
        Channel::Next => built.revision == target.revision,
    };
    if !matches || built.channel != target.channel {
        bail!("The staged Desktop bundle is not the release that was checked");
    }
    Ok(())
}

/// The installer's final `staged: <dir>/Phux.app` line, which must name our
/// staging directory.
pub(super) fn staged_path(stdout: &str, staging: &Path) -> anyhow::Result<PathBuf> {
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
pub(super) const WAIT_THEN_SWAP: &str = concat!(
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

pub(super) const SWAP: &str = concat!(
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

pub(super) fn apply_command(
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

pub(super) fn update_log() -> Stdio {
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
    if cfg!(target_os = "linux") {
        bail!("{}", linux_install_remedy(Channel::Stable, Some(bundle)));
    }
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
    if cfg!(target_os = "linux") {
        return;
    }
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
