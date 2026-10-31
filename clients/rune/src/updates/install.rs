//! Staging a verified update beside the app. A helper started at staging
//! waits for this process to exit, however it exits, and only then swaps the
//! bundle in, so quitting installs and a restart is a quit plus a flag.
//! Linux replaces the binary in place: the running one keeps its inode.

use std::fs::{self, File};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use phux_client_runtime::release::signature::{RELEASE_KEYS, signed_digest};
use sha2::{Digest, Sha256};

use super::{APP, CURL, EXECUTABLE, Release, fetch};

const STAGING: &str = ".phux-rune-update";
const LOCK: &str = ".phux-rune-update.lock";
/// In staging: reopen after the swap. Its text is the session to reopen.
const RELAUNCH: &str = "relaunch";
const IDENTIFIER: &str = "sh.phux.rune";
/// Far above any real archive, so a hostile feed can't fill the disk.
const MAX_ARCHIVE: &str = "1073741824";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Downloading(u8),
    Verifying,
}

fn staging(install: &Path) -> Result<PathBuf, String> {
    let parent = if cfg!(target_os = "macos") {
        install.parent().ok_or("The app has no parent directory.")?
    } else {
        install
    };
    Ok(parent.join(STAGING))
}

fn lock_file(staging: &Path) -> PathBuf {
    staging.with_file_name(LOCK)
}

fn failed(what: &str, error: impl std::fmt::Display) -> String {
    format!("{what}: {error}")
}

/// Require that a release key signed exactly the digest the feed publishes.
/// The feed is unsigned, so its digest alone would let a host that rewrites
/// release assets vouch for any archive.
fn check_signed(
    release: &Release,
    keys: &[&str],
    fetch: impl Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    let url = release.url();
    let sidecar = fetch(&format!("{url}.sha256"))
        .map_err(|error| failed("Could not download the checksum", error))?;
    let minisig = fetch(&format!("{url}.sha256.minisig")).map_err(|error| {
        failed(
            "No signature could be downloaded; nothing was installed",
            error,
        )
    })?;
    let signed = signed_digest(&sidecar, &minisig, &release.archive(), keys)
        .map_err(|error| error.to_string())?;
    if signed == release.sha256 {
        Ok(())
    } else {
        Err("The update feed and the signed checksum disagree; nothing was installed.".to_owned())
    }
}

fn check_digest(path: &Path, expected: &str) -> Result<(), String> {
    let mut file =
        File::open(path).map_err(|error| failed("Could not read the download", error))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| failed("Could not read the download", error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest == expected {
        Ok(())
    } else {
        Err("The download does not match its signed checksum; nothing was installed.".to_owned())
    }
}

/// Reports the file's real size against the published one as curl writes it.
fn download(release: &Release, to: &Path, report: &dyn Fn(Progress)) -> Result<(), String> {
    let mut child = Command::new(CURL)
        .args(["-fsSL", "--proto", "=https", "--connect-timeout", "10"])
        .args(["--max-time", "900", "--max-filesize", MAX_ARCHIVE, "-o"])
        .arg(to)
        .arg(release.url())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| failed("Could not start curl", error))?;
    let mut shown = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => return Err(failed("Waiting for curl", error)),
        }
        let bytes = fs::metadata(to).map_or(0, |meta| meta.len());
        let percent = (bytes.saturating_mul(100) / release.size).min(100) as u8;
        if shown != Some(percent) {
            shown = Some(percent);
            report(Progress::Downloading(percent));
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut message = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut message);
    }
    Err(failed("The download failed", message.trim()))
}

fn run(command: &mut Command, what: &str) -> Result<String, String> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|error| failed(what, error))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(failed(what, String::from_utf8_lossy(&output.stderr).trim()))
    }
}

fn plist(app: &Path, key: &str) -> Result<String, String> {
    run(
        Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", &format!("Print :{key}")])
            .arg(app.join("Contents/Info.plist")),
        "The update has no Info.plist",
    )
}

/// The app inside `archive`, verified, at `<staging>/Phux Rune.app`.
fn stage_macos(archive: &Path, dir: &Path, release: &Release) -> Result<(), String> {
    let unpacked = dir.join("unpacked");
    run(
        Command::new("/usr/bin/ditto")
            .arg("-x")
            .arg("-k")
            .arg(archive)
            .arg(&unpacked),
        "Could not unpack the update",
    )?;
    let app = unpacked.join(APP);
    run(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(&app),
        "The update's signature does not verify",
    )?;
    let names_itself = plist(&app, "CFBundleIdentifier")? == IDENTIFIER;
    if !names_itself || plist(&app, "CFBundleShortVersionString")? != release.version {
        return Err("The archive is not the release that was checked.".to_owned());
    }
    fs::rename(&app, dir.join(APP)).map_err(|error| failed("Could not stage the update", error))?;
    fs::remove_dir_all(&unpacked).map_err(|error| failed("Could not stage the update", error))?;
    fs::remove_file(archive).map_err(|error| failed("Could not stage the update", error))
}

/// Swap the binary now: one rename on the same volume.
fn install_linux(
    archive: &Path,
    dir: &Path,
    install: &Path,
    release: &Release,
) -> Result<(), String> {
    run(
        Command::new("tar")
            .arg("-xzf")
            .arg(archive)
            .arg("-C")
            .arg(dir),
        "Could not unpack the update",
    )?;
    let member = archive
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".tar.gz"))
        .ok_or("The archive has an unexpected name.")?;
    let binary = dir.join(member).join(EXECUTABLE);
    let reported = run(
        Command::new(&binary).arg("--version"),
        "The update does not run on this computer",
    )?;
    if !reported.ends_with(&release.version) {
        return Err("The archive is not the release that was checked.".to_owned());
    }
    fs::rename(&binary, install.join(EXECUTABLE))
        .map_err(|error| failed("Could not install the update", error))?;
    let _ = fs::remove_dir_all(dir);
    Ok(())
}

/// Download, verify, and make ready. Nothing outside the staging directory
/// changes before every check has passed.
pub fn stage(install: &Path, release: &Release, report: &dyn Fn(Progress)) -> Result<(), String> {
    check_signed(release, RELEASE_KEYS, fetch)?;
    let dir = staging(install)?;
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|error| failed("Could not write beside the app", error))?;
    let result = stage_in(install, &dir, release, report);
    if result.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    result
}

fn stage_in(
    install: &Path,
    dir: &Path,
    release: &Release,
    report: &dyn Fn(Progress),
) -> Result<(), String> {
    let archive = dir.join(release.archive());
    download(release, &archive, report)?;
    report(Progress::Verifying);
    check_digest(&archive, &release.sha256)?;
    if cfg!(target_os = "macos") {
        stage_macos(&archive, dir, release)?;
        swap_after_exit(
            install,
            &dir.join(APP),
            Path::new("/usr/bin/open"),
            std::process::id(),
        )
    } else {
        install_linux(&archive, dir, install, release)
    }
}

/// Every path arrives as a positional parameter, never as script text.
const RUNNING: &str = r#"running() {
  /bin/ps -axww -o command= | PHUX_EXE="$1/Contents/MacOS/phux-rune" /usr/bin/awk \
    'index($0, ENVIRON["PHUX_EXE"]) == 1 { found = 1 } END { exit !found }'
}
"#;

const WAIT: &str = r#"pid=$1; lock=$2; staged=$3; installed=$4; opener=$5; swap=$6
while kill -0 "$pid" 2>/dev/null; do sleep 1; done
exec /usr/bin/lockf -s -t 30 "$lock" /bin/sh -c "$swap" phux-swap "$staged" "$installed" "$opener""#;

const SWAP: &str = r#"staged=$1; installed=$2; opener=$3
staging=${staged%/*}
previous="$staging/previous.app"
if [ ! -d "$staged" ]; then
  echo "no staged update at $staged; nothing changed"
  exit 1
fi
# An instance opened during the wait keeps its bundle.
if running "$installed"; then
  echo "Phux Rune is running again; the update waits for the next launch"
  exit 1
fi
relaunch=0
if [ -f "$staging/relaunch" ]; then
  relaunch=1
  session=$(cat "$staging/relaunch")
fi
if [ -e "$installed" ] && ! mv "$installed" "$previous"; then
  echo "could not move the installed app aside; nothing changed"
  exit 1
fi
if ! mv "$staged" "$installed"; then
  [ ! -e "$previous" ] || mv "$previous" "$installed"
  echo "could not install the update; the previous app is restored"
  exit 1
fi
rm -rf "$staging"
/usr/bin/xattr -d com.apple.quarantine "$installed" 2>/dev/null || true
echo "installed the update at $installed"
[ "$relaunch" = 1 ] || exit 0
if [ -n "$session" ]; then
  exec "$opener" "$installed" --args --session "$session"
fi
exec "$opener" "$installed""#;

fn log() -> Stdio {
    let Some(home) = std::env::var_os("HOME") else {
        return Stdio::null();
    };
    let dir = PathBuf::from(home).join("Library/Logs/Phux");
    let _ = fs::create_dir_all(&dir);
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("rune-update.log"))
        .map_or_else(|_| Stdio::null(), Stdio::from)
}

fn swap_after_exit(install: &Path, staged: &Path, opener: &Path, pid: u32) -> Result<(), String> {
    let dir = staged.parent().ok_or("The staged app has no parent.")?;
    Command::new("/bin/sh")
        .args(["-c", WAIT, "phux-update", &pid.to_string()])
        .arg(lock_file(dir))
        .arg(staged)
        .arg(install)
        .arg(opener)
        .arg(format!("{RUNNING}{SWAP}"))
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(log())
        .stderr(log())
        .spawn()
        .map_err(|error| failed("Could not start the update helper", error))?;
    Ok(())
}

/// Ask for the updated app to reopen on `session` once this process exits.
pub fn relaunch_after_exit(install: &Path, session: Option<&str>) -> Result<(), String> {
    if cfg!(target_os = "macos") {
        let flag = staging(install)?.join(RELAUNCH);
        return fs::write(flag, session.unwrap_or_default())
            .map_err(|error| failed("Could not ask for a restart", error));
    }
    Command::new("/bin/sh")
        .args([
            "-c",
            r#"pid=$1; shift; while kill -0 "$pid" 2>/dev/null; do sleep 0.2; done; exec "$@""#,
            "phux-relaunch",
            &std::process::id().to_string(),
        ])
        .arg(install.join(EXECUTABLE))
        .args(
            session
                .into_iter()
                .flat_map(|session| ["--session", session]),
        )
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| failed("Could not restart", error))?;
    Ok(())
}

/// A staged update whose helper never ran (a reboot took it) is discarded at
/// launch, unless a helper is swapping right now.
pub fn clear_stale(install: &Path) {
    let Ok(dir) = staging(install) else { return };
    if !dir.exists() {
        return;
    }
    if !cfg!(target_os = "macos") {
        let _ = fs::remove_dir_all(dir);
        return;
    }
    let _ = Command::new("/usr/bin/lockf")
        .args(["-s", "-t", "0"])
        .arg(lock_file(&dir))
        .arg("/bin/rm")
        .arg("-rf")
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    /// A throwaway key that signed only the fixture.
    const KEY: &str = "RWS1x8Dpy+4AOckENR1h3WuFzrawqASGwRG5ZPd2nHUGXSG6vX0ORnFK";

    #[cfg(target_os = "macos")]
    fn release(archive_sha: &str) -> Release {
        Release {
            version: "0.1.0-alpha.9".to_owned(),
            sha256: archive_sha.to_owned(),
            size: 1,
            protocol: "1.0".to_owned(),
        }
    }

    #[cfg(target_os = "macos")]
    fn signed_files(url: &str) -> Result<Vec<u8>, String> {
        let sidecar = include_bytes!("testdata/archive.sha256");
        let minisig = include_bytes!("testdata/archive.sha256.minisig");
        if url.ends_with(".minisig") {
            Ok(minisig.to_vec())
        } else {
            Ok(sidecar.to_vec())
        }
    }

    #[cfg(target_os = "macos")]
    fn published() -> String {
        let sidecar = include_str!("testdata/archive.sha256");
        sidecar.split_whitespace().next().unwrap().to_owned()
    }

    // The fixture names the macOS archive.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_signed_digest_that_matches_passes() {
        check_signed(&release(&published()), &[KEY], signed_files).unwrap();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_feed_cannot_name_another_digest() {
        let error = check_signed(&release(&"f".repeat(64)), &[KEY], signed_files).unwrap_err();
        assert!(error.contains("disagree"), "{error}");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_foreign_key_is_refused() {
        // The real release keys never signed the fixture.
        assert!(check_signed(&release(&published()), RELEASE_KEYS, signed_files).is_err());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_missing_signature_refuses() {
        let result = check_signed(&release(&published()), &[KEY], |url: &str| {
            if url.ends_with(".minisig") {
                Err("404".to_owned())
            } else {
                signed_files(url)
            }
        });
        assert!(result.unwrap_err().contains("No signature"));
    }

    #[test]
    fn the_download_must_match_its_digest() {
        let dir = temp("digest");
        let file = dir.join("archive");
        fs::write(&file, b"phux").unwrap();
        assert!(
            check_digest(&file, &"0".repeat(64))
                .unwrap_err()
                .contains("does not match")
        );
        let digest = Sha256::digest(b"phux");
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        check_digest(&file, &hex).unwrap();
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("phux-rune-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    /// A fake app whose marker says which build it is.
    #[cfg(target_os = "macos")]
    fn app(at: &Path, marker: &str) {
        fs::create_dir_all(at.join("Contents/MacOS")).unwrap();
        fs::write(at.join("marker"), marker).unwrap();
    }

    #[cfg(target_os = "macos")]
    fn swap(apps: &Path, opener: &Path, relaunch: Option<&str>) -> std::process::Output {
        let installed = apps.join(APP);
        let dir = apps.join(STAGING);
        fs::create_dir_all(&dir).unwrap();
        app(&installed, "old");
        app(&dir.join(APP), "new");
        if let Some(session) = relaunch {
            fs::write(dir.join(RELAUNCH), session).unwrap();
        }
        Command::new("/bin/sh")
            .args(["-c", &format!("{RUNNING}{SWAP}"), "phux-swap"])
            .arg(dir.join(APP))
            .arg(&installed)
            .arg(opener)
            .output()
            .unwrap()
    }

    /// Records its arguments, one per line.
    #[cfg(target_os = "macos")]
    fn recorder(dir: &Path) -> (PathBuf, PathBuf) {
        let script = dir.join("open.sh");
        let out = dir.join("opened");
        fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", out.display()),
        )
        .unwrap();
        run(Command::new("/bin/chmod").arg("+x").arg(&script), "chmod").unwrap();
        (script, out)
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn quitting_swaps_the_app_without_reopening_it() {
        let apps = temp("quit");
        let (opener, opened) = recorder(&apps);
        let output = swap(&apps, &opener, None);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(fs::read(apps.join(APP).join("marker")).unwrap(), b"new");
        assert!(!apps.join(STAGING).exists());
        assert!(!opened.exists());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_restart_reopens_the_session() {
        let apps = temp("restart");
        let (opener, opened) = recorder(&apps);
        let output = swap(&apps, &opener, Some("my work"));
        assert!(output.status.success(), "{output:?}");
        let args = fs::read_to_string(opened).unwrap();
        let expected = format!("{}\n--args\n--session\nmy work\n", apps.join(APP).display());
        assert_eq!(args, expected);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_failed_swap_keeps_the_old_app() {
        let apps = temp("missing");
        app(&apps.join(APP), "old");
        fs::create_dir_all(apps.join(STAGING)).unwrap();
        let output = Command::new("/bin/sh")
            .args(["-c", &format!("{RUNNING}{SWAP}"), "phux-swap"])
            .arg(apps.join(STAGING).join(APP))
            .arg(apps.join(APP))
            .arg("/usr/bin/true")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(fs::read(apps.join(APP).join("marker")).unwrap(), b"old");
    }
}
