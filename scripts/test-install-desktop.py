#!/usr/bin/env python3
"""scripts/install.sh --desktop on macOS arm64: real zips, real ad-hoc
signatures and a real lockf lock, with a fake network."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[1]
INSTALLER = ROOT / "scripts/install.sh"
GOLDEN = ROOT / "scripts/fixtures/release-manifest"
VERSION = "0.1.0"
SHELL = shutil.which("dash") or "/bin/sh"
FAKE_CURL = r'''#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in -o) out="$2"; shift ;; https:*) url="$1" ;; esac
  shift
done
printf '%s\n' "$url" >> "$FIXTURE/calls"
case "$url" in
  */channels/*.json) file="$FIXTURE/manifest.json" ;;
  *SHA256SUMS | *.zip.sha256) file="$FIXTURE/SHA256SUMS" ;;
  *.zip) file="$FIXTURE/asset.zip" ;;
  *.tar.gz) file="$FIXTURE/cli.tar.gz" ;;
  *) echo "unexpected URL: $url" >&2; exit 1 ;;
esac
[ -f "$file" ] || exit 22
cp "$file" "$out"
'''


def run(*args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs)


def macos_27_arm64():
    return (platform.system() == "Darwin" and platform.machine() == "arm64"
            and int(platform.mac_ver()[0].split(".")[0]) >= 27)


@unittest.skipUnless(macos_27_arm64(), "Phux Desktop installs on macOS 27+ arm64")
class DesktopInstaller(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="phux-desktop-install-test-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.fixture = self.root / "fixture"
        self.bin = self.root / "bin"
        self.apps = self.root / "Applications with spaces"
        self.launchers = self.root / "launchers"
        for directory in (self.fixture, self.bin, self.launchers):
            directory.mkdir()
        self.app = self.root / "source/Phux.app"
        (self.app / "Contents/MacOS").mkdir(parents=True)
        source = self.root / "main.c"
        source.write_text("int main(void) { return 0; }\n")
        run("cc", str(source), "-o", str(self.app / "Contents/MacOS/phux-desktop"))
        self.plist = {"CFBundleIdentifier": "dev.phux.desktop", "CFBundleExecutable": "phux-desktop",
                      "CFBundlePackageType": "APPL", "PhuxDesktopVersion": VERSION}
        self.asset = f"phux-desktop-{VERSION}-macos-arm64.zip"
        self.pack()
        (self.bin / "curl").write_text(FAKE_CURL)
        (self.bin / "curl").chmod(0o755)
        # A CLI is already installed unless a test removes it.
        (self.launchers / "phux").symlink_to("/usr/bin/true")
        self.env = {"HOME": str(self.root / "home"), "FIXTURE": str(self.fixture),
                    "PATH": f"{self.bin}:/usr/bin:/bin:/usr/sbin:/sbin"}

    def pack(self):
        (self.app / "Contents/Info.plist").write_bytes(plistlib.dumps(self.plist))
        run("codesign", "--force", "--sign", "-", str(self.app))
        archive = self.fixture / "asset.zip"
        archive.unlink(missing_ok=True)
        run("ditto", "-c", "-k", "--keepParent", "--norsrc", str(self.app), str(archive))
        (self.fixture / "SHA256SUMS").write_text(f"{self.digest}  {self.asset}\n")

    @property
    def digest(self):
        return hashlib.sha256((self.fixture / "asset.zip").read_bytes()).hexdigest()

    def manifest(self, channel, sha="c" * 40, digest=None):
        document = json.loads((GOLDEN / f"{channel}.json").read_text())
        document["products"]["desktop"].update(version=VERSION, sha=sha)
        document["products"]["desktop"]["sha256"]["aarch64-apple-darwin"] = digest or self.digest
        (self.fixture / "manifest.json").write_text(json.dumps(document, indent=2) + "\n")

    def next_build(self):
        sha = "a" * 40
        self.asset = f"phux-desktop-next.{sha}-macos-arm64.zip"
        self.plist.update(PhuxBuildSHA=sha, PhuxDesktopChannel="next")
        self.pack()
        self.manifest("next", sha)
        return sha

    def install(self, *args, ok=True):
        result = subprocess.run(
            [SHELL, str(INSTALLER), "--desktop", "--applications-dir", str(self.apps),
             "--install-dir", str(self.launchers), *(args or ("--version", VERSION))],
            env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, ok, result.stdout + result.stderr)
        return result

    def stage(self, *args, ok=True):
        result = subprocess.run([SHELL, str(INSTALLER), "--desktop", *args],
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, ok, result.stdout + result.stderr)
        return result

    @property
    def installed(self):
        return self.apps / "Phux.app"

    def installed_plist(self):
        return plistlib.loads((self.installed / "Contents/Info.plist").read_bytes())

    @property
    def calls(self):
        path = self.fixture / "calls"
        return path.read_text().splitlines() if path.exists() else []

    def test_install_update_and_launcher(self):
        self.install()
        run("codesign", "--verify", "--deep", "--strict", str(self.installed))
        self.plist["TestBuild"] = "updated"
        self.pack()
        self.install()
        self.assertEqual(self.installed_plist()["TestBuild"], "updated")
        self.assertTrue(os.access(self.launchers / "phux-desktop", os.X_OK))

    def test_unpinned_installs_verify_the_manifest_digest(self):
        self.manifest("stable")
        (self.fixture / "SHA256SUMS").unlink()
        self.assertIn(f"installed Phux {VERSION}", self.install("--channel", "stable").stderr)
        self.assertFalse(any("SHA256SUMS" in url for url in self.calls))
        before = self.installed_plist()
        self.manifest("stable", digest="0" * 64)
        self.plist["TestBuild"] = "must-not-install"
        self.pack()
        self.assertIn("checksum mismatch", self.install("--channel", "stable", ok=False).stderr)
        self.assertEqual(self.installed_plist(), before)

    def test_next_checks_the_bundle_commit_and_channel(self):
        sha = self.next_build()
        self.assertIn(f"installed Phux {sha}", self.install("--channel", "next").stderr)
        self.plist["PhuxBuildSHA"] = "b" * 40
        self.pack()
        self.manifest("next", sha)
        self.assertIn("is not commit", self.install("--channel", "next", ok=False).stderr)
        self.assertEqual(self.installed_plist()["PhuxBuildSHA"], sha)
        self.plist.update(PhuxBuildSHA=sha, PhuxDesktopChannel="stable")
        self.pack()
        self.assertIn("not a next build", self.install("--channel", "next", "--revision", sha, ok=False).stderr)

    def test_a_revision_pin_reads_no_manifest(self):
        sha = self.next_build()
        (self.fixture / "manifest.json").unlink()
        self.install("--channel", "next", "--revision", sha)
        self.assertFalse(any("/channels/" in url for url in self.calls))

    def test_the_bundle_must_be_the_requested_stable_release(self):
        self.plist["PhuxDesktopChannel"] = "next"
        self.pack()
        self.assertIn("not a stable build", self.install(ok=False).stderr)
        self.plist.update(PhuxDesktopChannel="stable", PhuxDesktopVersion="0.1.1")
        self.pack()
        self.assertIn("is not version", self.install(ok=False).stderr)
        self.assertFalse(self.apps.exists())

    def test_a_missing_cli_is_installed_first(self):
        (self.launchers / "phux").unlink()
        cli = self.root / "cli"
        cli.mkdir()
        for binary in ("phux", "phux-mcp"):
            (cli / binary).write_text("#!/bin/sh\n")
            (cli / binary).chmod(0o755)
        run("bash", str(ROOT / "scripts/pack-release.sh"), "--id", "v9.8.7", "--target",
            "aarch64-apple-darwin", "--bin-dir", str(cli), "--out-dir", str(self.fixture))
        archive = self.fixture / "phux-v9.8.7-aarch64-apple-darwin.tar.gz"
        archive.rename(self.fixture / "cli.tar.gz")
        self.manifest("stable")
        document = json.loads((self.fixture / "manifest.json").read_text())
        document["products"]["phux"]["sha256"]["aarch64-apple-darwin"] = hashlib.sha256(
            (self.fixture / "cli.tar.gz").read_bytes()).hexdigest()
        (self.fixture / "manifest.json").write_text(json.dumps(document, indent=2) + "\n")
        result = self.install("--channel", "stable")
        self.assertIn("phux CLI not found; installing it first", result.stderr)
        self.assertTrue(os.access(self.launchers / "phux", os.X_OK))
        self.assertTrue(self.installed.is_dir())

    def test_a_failed_cli_install_installs_no_app(self):
        (self.launchers / "phux").unlink()
        self.manifest("stable")
        self.install("--channel", "stable", ok=False)
        self.assertFalse(self.installed.exists())

    def test_staging_touches_nothing_but_the_stage(self):
        self.install()
        before = self.installed_plist()
        launcher = (self.launchers / "phux-desktop").read_bytes()
        (self.launchers / "phux").unlink()
        (self.fixture / "SHA256SUMS").unlink()
        stage = self.apps / ".phux-desktop-update"
        for build in ("first", "second"):
            self.plist["TestBuild"] = build
            self.pack()
            result = self.stage("--stage-dir", f"{stage}/", "--version", VERSION, "--sha256", self.digest)
            self.assertEqual(result.stdout.splitlines()[-1], f"staged: {stage}/Phux.app")
            staged = plistlib.loads((stage / "Phux.app/Contents/Info.plist").read_bytes())
            self.assertEqual(staged["TestBuild"], build)
        self.assertEqual(sorted(path.name for path in stage.iterdir()), ["Phux.app"])
        self.assertEqual(self.installed_plist(), before)
        self.assertEqual((self.launchers / "phux-desktop").read_bytes(), launcher)
        self.assertFalse((self.launchers / "phux").exists())

    def test_staging_refusals(self):
        sha = self.next_build()
        stage = self.apps / ".phux-desktop-update"
        self.stage("--stage-dir", str(stage), "--channel", "next", "--revision", sha, "--sha256", self.digest)
        before = (stage / "Phux.app/Contents/Info.plist").read_bytes()
        for args, message in (
            (["--stage-dir", "relative/dir", "--version", VERSION], "absolute path"),
            (["--stage-dir", str(stage), "--sha256", self.digest], "--sha256 needs"),
            (["--stage-dir", str(stage), "--channel", "next", "--revision", sha, "--sha256", "0" * 64],
             "checksum mismatch"),
        ):
            with self.subTest(args=args):
                self.assertIn(message, self.stage(*args, ok=False).stderr)
        with (self.apps / ".phux-desktop-install.lock").open("w") as owner:
            fcntl.flock(owner, fcntl.LOCK_EX)
            result = self.stage("--stage-dir", str(stage), "--channel", "next", "--revision", sha,
                                "--sha256", self.digest, ok=False)
        self.assertIn("another Desktop install is publishing", result.stderr)
        self.assertEqual((stage / "Phux.app/Contents/Info.plist").read_bytes(), before)

    def test_a_symlink_in_the_archive_is_refused(self):
        with zipfile.ZipFile(self.fixture / "asset.zip", "a") as archive:
            link = zipfile.ZipInfo("Phux.app/escape")
            link.create_system = 3
            link.external_attr = 0o120777 << 16
            archive.writestr(link, "../../outside")
        (self.fixture / "SHA256SUMS").write_text(f"{self.digest}  {self.asset}\n")
        self.assertIn("symlinks", self.install(ok=False).stderr)
        self.assertFalse(self.installed.exists())

    def test_a_failed_publish_restores_the_previous_app(self):
        self.install()
        before = self.installed_plist()
        self.plist["TestBuild"] = "must-not-publish"
        self.pack()
        (self.bin / "mv").write_text(
            '#!/bin/sh\ncase "$1" in */.phux-desktop-install.*/Phux.app) exit 1 ;; esac\nexec /bin/mv "$@"\n')
        (self.bin / "mv").chmod(0o755)
        self.install(ok=False)
        self.assertEqual(self.installed_plist(), before)
        self.assertEqual(list(self.apps.glob(".phux-desktop-install.*")), [self.apps / ".phux-desktop-install.lock"])

    def test_an_unrelated_app_is_never_replaced(self):
        self.installed.mkdir(parents=True)
        (self.installed / "not-phux").write_text("keep me")
        self.assertIn("unrelated", self.install(ok=False).stderr)
        self.assertEqual((self.installed / "not-phux").read_text(), "keep me")

    def test_a_dead_lock_holder_does_not_block_and_a_live_one_does(self):
        self.install()
        lock = self.apps / ".phux-desktop-install.lock"
        owner = subprocess.Popen([sys.executable, "-c",
                                  "import fcntl, os, signal, sys; f = open(sys.argv[1], 'w'); "
                                  "fcntl.flock(f, fcntl.LOCK_EX); os.kill(os.getpid(), signal.SIGKILL)",
                                  str(lock)])
        self.assertEqual(owner.wait(), -9)
        self.install()
        with lock.open("w") as holder:
            fcntl.flock(holder, fcntl.LOCK_EX)
            self.assertIn("another Desktop install is publishing", self.install(ok=False).stderr)


if __name__ == "__main__":
    unittest.main()
