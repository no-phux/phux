use super::*;

/// A throwaway key that signed only the fixture; the fixture names this
/// platform's archive, as `Release::archive` does.
#[cfg(target_os = "macos")]
const KEY: &str = "RWS1x8Dpy+4AOckENR1h3WuFzrawqASGwRG5ZPd2nHUGXSG6vX0ORnFK";
#[cfg(target_os = "macos")]
const SIDECAR: &[u8] = include_bytes!("../testdata/archive.sha256");
#[cfg(target_os = "macos")]
const MINISIG: &[u8] = include_bytes!("../testdata/archive.sha256.minisig");

const LINUX_KEY: &str = "RWSNytjkzxeEuiBHgsO/1+Le7y35w7t3NTYXtwvRGj+aNSXtaQFHCeV5";
const LINUX_ARCHIVE: &str = "phux-rune-0.1.0-alpha.9-linux-x86_64.tar.gz";
const LINUX_SIDECAR: &[u8] = include_bytes!("../testdata/archive-linux.sha256");
const LINUX_MINISIG: &[u8] = include_bytes!("../testdata/archive-linux.sha256.minisig");

#[cfg(not(target_os = "macos"))]
const KEY: &str = LINUX_KEY;
#[cfg(not(target_os = "macos"))]
const SIDECAR: &[u8] = LINUX_SIDECAR;
#[cfg(not(target_os = "macos"))]
const MINISIG: &[u8] = LINUX_MINISIG;

fn release(archive_sha: &str) -> Release {
    Release {
        version: "0.1.0-alpha.9".to_owned(),
        sha256: archive_sha.to_owned(),
        size: 1,
        protocol: "1.0".to_owned(),
    }
}

#[allow(clippy::unnecessary_wraps)]
fn signed_files(url: &str) -> Result<Vec<u8>, String> {
    if url.ends_with(".minisig") {
        Ok(MINISIG.to_vec())
    } else {
        Ok(SIDECAR.to_vec())
    }
}

fn published() -> String {
    let sidecar = String::from_utf8_lossy(SIDECAR);
    sidecar.split_whitespace().next().unwrap().to_owned()
}

#[test]
fn a_signed_digest_that_matches_passes() {
    check_signed(&release(&published()), &[KEY], signed_files).unwrap();
}

/// Holds on every host, so a macOS run can't break the Linux fixture unseen.
#[test]
fn the_linux_fixture_is_signed_for_the_linux_archive() {
    let signed = signed_digest(LINUX_SIDECAR, LINUX_MINISIG, LINUX_ARCHIVE, &[LINUX_KEY]).unwrap();
    assert_eq!(signed.len(), 64);
    assert!(signed_digest(LINUX_SIDECAR, LINUX_MINISIG, "other.tar.gz", &[LINUX_KEY]).is_err());
}

#[test]
fn the_feed_cannot_name_another_digest() {
    let error = check_signed(&release(&"f".repeat(64)), &[KEY], signed_files).unwrap_err();
    assert!(error.contains("disagree"), "{error}");
}

#[test]
fn a_foreign_key_is_refused() {
    // The real release keys never signed the fixture.
    assert!(check_signed(&release(&published()), RELEASE_KEYS, signed_files).is_err());
}

#[test]
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
fn a_held_update_stays_out_on_quit_but_not_on_restart() {
    let apps = temp("hold");
    let (opener, _) = recorder(&apps);
    fs::create_dir_all(apps.join(STAGING)).unwrap();
    fs::write(apps.join(STAGING).join(HOLD), "").unwrap();
    let output = swap(&apps, &opener, None);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read(apps.join(APP).join("marker")).unwrap(), b"old");
    let output = swap(&apps, &opener, Some("work"));
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read(apps.join(APP).join("marker")).unwrap(), b"new");
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

/// `what` is the body of a stand-in binary: `$1` is its first argument.
fn fake_binary(path: &Path, what: &str) {
    fs::write(path, format!("#!/bin/sh\n{what}\n")).unwrap();
    run(Command::new("/bin/chmod").arg("+x").arg(path), "chmod").unwrap();
}

/// Answers `--version`, and draws a frame where `--snap` says.
const HEALTHY: &str = r#"[ "$1" = --version ] && { echo "phux-rune 0.1.0-alpha.9"; exit 0; }
while [ "$#" -gt 0 ]; do [ "$1" = --snap ] && echo frame > "$2"; shift; done"#;

/// A Linux install of `old`, and an archive of `body` beside it.
fn linux_update(name: &str, body: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = temp(name);
    let install = root.join("phux-rune");
    fs::create_dir_all(&install).unwrap();
    fs::write(install.join(EXECUTABLE), "old").unwrap();
    fs::write(install.join("NOTICE"), "old notice").unwrap();
    let packed = root.join("packed");
    let member = packed.join("phux-rune-0.1.0-alpha.9-linux-x86_64");
    fs::create_dir_all(&member).unwrap();
    fake_binary(&member.join(EXECUTABLE), body);
    fs::write(member.join("NOTICE"), "new notice").unwrap();
    fs::write(member.join("phux-rune.release"), "0.1.0-alpha.9").unwrap();
    let dir = root.join("staging");
    fs::create_dir_all(&dir).unwrap();
    let archive = dir.join(LINUX_ARCHIVE);
    run(
        Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&packed)
            .arg(member.file_name().unwrap()),
        "tar",
    )
    .unwrap();
    (archive, dir, install)
}

#[test]
fn linux_swaps_the_binary_and_its_files_at_staging() {
    let (archive, dir, install) = linux_update("linux-swap", HEALTHY);
    install_linux(&archive, &dir, &install, &release("")).unwrap();
    assert!(
        fs::read_to_string(install.join(EXECUTABLE))
            .unwrap()
            .contains("--version")
    );
    assert_eq!(fs::read(install.join("NOTICE")).unwrap(), b"new notice");
    assert!(install.join("phux-rune.release").is_file());
    assert!(!dir.exists());
}

#[test]
fn linux_refuses_another_version() {
    let (archive, dir, install) = linux_update("linux-version", "echo phux-rune 0.0.1");
    let error = install_linux(&archive, &dir, &install, &release("")).unwrap_err();
    assert!(error.contains("not the release"), "{error}");
    assert_eq!(fs::read(install.join(EXECUTABLE)).unwrap(), b"old");
    assert_eq!(fs::read(install.join("NOTICE")).unwrap(), b"old notice");
}

#[test]
fn linux_refuses_a_build_that_dies_at_launch() {
    let dies = r#"[ "$1" = --version ] && { echo "phux-rune 0.1.0-alpha.9"; exit 0; }; exit 101"#;
    let (archive, dir, install) = linux_update("linux-crash", dies);
    let error = install_linux(&archive, &dir, &install, &release("")).unwrap_err();
    assert!(error.contains("does not start"), "{error}");
    assert_eq!(fs::read(install.join(EXECUTABLE)).unwrap(), b"old");
    assert_eq!(fs::read(install.join("NOTICE")).unwrap(), b"old notice");
}

#[test]
fn the_launch_check_needs_a_frame_not_just_an_exit() {
    let dir = temp("no-frame");
    let binary = dir.join("binary");
    fake_binary(&binary, "exit 0");
    let error = launch_check(&binary, &dir, LAUNCH_TIMEOUT).unwrap_err();
    assert!(error.contains("does not start"), "{error}");
}

#[test]
fn a_launch_that_hangs_is_killed_and_refused() {
    let dir = temp("hang");
    let binary = dir.join("binary");
    fake_binary(&binary, "exec sleep 30");
    let started = Instant::now();
    let error = launch_check(&binary, &dir, Duration::from_millis(200)).unwrap_err();
    assert!(error.contains("never finished"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_healthy_launch_passes() {
    let dir = temp("healthy");
    let binary = dir.join("binary");
    fake_binary(&binary, HEALTHY);
    launch_check(&binary, &dir, LAUNCH_TIMEOUT).unwrap();
}

#[test]
fn outcomes_read_as_a_log_line() {
    let at = Path::new("/opt/phux-rune");
    assert!(outcome(at, "1.0.0", &Err("offline".to_owned())).contains("failed: offline"));
    let done = outcome(at, "1.0.0", &Ok(()));
    assert!(
        done.contains("1.0.0") && done.contains("/opt/phux-rune"),
        "{done}"
    );
}
