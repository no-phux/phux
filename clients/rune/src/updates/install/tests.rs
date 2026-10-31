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
    let sidecar = include_bytes!("../testdata/archive.sha256");
    let minisig = include_bytes!("../testdata/archive.sha256.minisig");
    if url.ends_with(".minisig") {
        Ok(minisig.to_vec())
    } else {
        Ok(sidecar.to_vec())
    }
}

#[cfg(target_os = "macos")]
fn published() -> String {
    let sidecar = include_str!("../testdata/archive.sha256");
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
