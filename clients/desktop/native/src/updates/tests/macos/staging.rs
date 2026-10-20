//! Signed release staging against a fake network: every refusal leaves the
//! installed app alone.

use super::*;

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
        std::fs::set_permissions(bin.join("curl"), std::fs::Permissions::from_mode(0o755)).unwrap();
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

    /// Stage `target` with the signature gate passed; the gate itself is
    /// exercised by the `check_signed` tests and
    /// [`an_unsigned_digest_is_refused_before_the_installer_runs`].
    fn stage(&self, target: &Release) -> anyhow::Result<PathBuf> {
        self.stage_gated(target, |_| Ok(()))
    }

    fn stage_gated(
        &self,
        target: &Release,
        signed: impl FnOnce(&Release) -> anyhow::Result<()>,
    ) -> anyhow::Result<PathBuf> {
        stage_with(&self.bundle, target, signed, |command| {
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
        let error = self.stage(target).unwrap_err();
        self.assert_left_alone(&error, message);
    }

    fn assert_left_alone(self, error: &anyhow::Error, message: &str) {
        let error = format!("{error:#}");
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

/// A throwaway minisign key (not a release key) and its signature over
/// the golden stable manifest's Desktop checksum, made with the minisign
/// CLI.
const FIXTURE_KEY: &str = "RWQSnC835onn6wYhJSMsOtOnBQzqog7tkuPsxUYn++QJQkTWfVA9xo8k";
const FIXTURE_SIDECAR: &[u8] = include_bytes!("../../../testdata/stable.sha256");
const FIXTURE_MINISIG: &[u8] = include_bytes!("../../../testdata/stable.sha256.minisig");
const FIXTURE_ASSET: &str = "https://github.com/no-phux/phux/releases/download/desktop-v1.2.3/phux-desktop-1.2.3-macos-arm64.zip";

/// Serves the fixture sidecar and, unless `signed` is false, its
/// signature; anything else is a 404.
fn fixture_fetch(signed: bool) -> impl Fn(&str) -> anyhow::Result<Vec<u8>> {
    move |url| {
        if url == format!("{FIXTURE_ASSET}.sha256") {
            Ok(FIXTURE_SIDECAR.to_vec())
        } else if signed && url == format!("{FIXTURE_ASSET}.sha256.minisig") {
            Ok(FIXTURE_MINISIG.to_vec())
        } else {
            bail!("curl: (22) The requested URL returned error: 404")
        }
    }
}

#[test]
fn a_signed_digest_that_matches_the_manifest_is_trusted() {
    let target = manifest_release(STABLE, Channel::Stable).unwrap();
    check_signed(&target, &[FIXTURE_KEY], fixture_fetch(true)).unwrap();
}

#[test]
fn a_forged_manifest_digest_is_refused() {
    let mut target = manifest_release(STABLE, Channel::Stable).unwrap();
    target.sha256 = "e".repeat(64);
    let error = check_signed(&target, &[FIXTURE_KEY], fixture_fetch(true)).unwrap_err();
    assert!(
        format!("{error:#}").contains("but the signed checksum is"),
        "{error:#}"
    );
}

#[test]
fn a_missing_or_foreign_signature_is_refused() {
    let target = manifest_release(STABLE, Channel::Stable).unwrap();
    let error = check_signed(&target, &[FIXTURE_KEY], fixture_fetch(false)).unwrap_err();
    assert!(
        format!("{error:#}").contains("an unsigned release is never installed"),
        "{error:#}"
    );
    // The shipped keys do not trust the fixture key.
    let error = check_signed(&target, RELEASE_KEYS, fixture_fetch(true)).unwrap_err();
    assert!(format!("{error:#}").contains("does not trust"), "{error:#}");
}

/// The archive matches the manifest, but the manifest's digest is not the
/// signed one: the installer never runs and nothing is staged.
#[test]
fn an_unsigned_digest_is_refused_before_the_installer_runs() {
    let network = Network::new("stage-unsigned");
    let target = network.checked(&network.publish("dev.phux.desktop", |_| {}));
    let error = network
        .stage_gated(&target, |target| {
            check_signed(target, &[FIXTURE_KEY], fixture_fetch(true))
        })
        .unwrap_err();
    network.assert_left_alone(&error, "but the signed checksum is");
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
