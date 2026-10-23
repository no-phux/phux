//! The macOS bundle checks and the post-exit swap helper.

use super::*;

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
    assert!(stage(bundle, &Release::default(), std::sync::mpsc::channel().0).is_err());
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
    let staged = stage(
        &apps.join("Phux.app"),
        &target,
        std::sync::mpsc::channel().0,
    )
    .unwrap();
    assert_eq!(staged, apps.join(".phux-desktop-update/Phux.app"));
    assert_eq!(installed_release(&staged).unwrap().version, version);
    let mut wrong = target.clone();
    wrong.sha256 = "0".repeat(64);
    assert!(stage(&apps.join("Phux.app"), &wrong, std::sync::mpsc::channel().0).is_err());
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

mod staging;
