#!/usr/bin/env python3
"""Exercise the standalone desktop resolver and transactional macOS installation."""
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
INSTALLER = ROOT / "scripts/install-desktop.sh"
VERSION = "0.1.0"
ASSET = f"phux-desktop-{VERSION}-macos-arm64.zip"
SHELL = shutil.which("dash") or "/bin/sh"


def run(*args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, **kwargs)


class StableResolver(unittest.TestCase):
    def test_stable_is_default_and_drafts_and_prereleases_never_install(self):
        releases = [
            {"tag_name": "desktop-v9.0.0", "draft": True, "prerelease": False},
            {"tag_name": "desktop-v0.1.0", "draft": False, "prerelease": False},
            {"tag_name": "desktop-v0.2.0", "draft": False, "prerelease": True},
            {"tag_name": "desktop-v0.1.0-alpha.2", "draft": False, "prerelease": True},
            {"tag_name": "v8.0.0", "draft": False, "prerelease": False},
        ]
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory) / "releases.json"
            fixture.write_text(json.dumps(releases))
            script = '. "$1"; release_page "$2" "$3" "$4"'
            lib = str(ROOT / "scripts/lib/install-release.sh")
            desktop = run(SHELL, "-c", script, "sh", lib, "desktop-v", str(fixture), "stable")
            core = run(SHELL, "-c", script, "sh", lib, "v", str(fixture), "stable")
            self.assertEqual(desktop.stdout.strip(), "desktop-v0.1.0")
            self.assertEqual(core.stdout.strip(), "v8.0.0")

    @unittest.skipUnless(
        platform.system() == "Darwin" and platform.machine() == "arm64",
        "standalone desktop resolver requires macOS arm64",
    )
    def test_standalone_selects_highest_stable_across_pages(self):
        if int(run("/usr/bin/sw_vers", "-productVersion").stdout.split(".")[0]) < 27:
            self.skipTest("desktop requires macOS 27 or later")

        def release(version, **metadata):
            return {"tag_name": f"desktop-v{version}", "draft": False, "prerelease": False, **metadata}

        cases = [
            ([[
                release("0.10.0"),
                release("0.11.0"),
                release("0.9.0"),
            ]], "0.11.0"),
            ([
                [release("0.9.0")],
                [
                    release("0.8.0"),
                    release("0.10.0"),
                    release("99.0.0", draft=True),
                    release("99.0.0", prerelease=True),
                    release("99.0.0", tag_name="v99.0.0"),
                    release("99.0.0", tag_name="cockpit-v99.0.0"),
                ],
                [release("0.7.0")],
            ], "0.10.0"),
            ([[
                release("0.999.0"),
                release("1.0.0"),
                release("0.1000.0"),
            ]], "1.0.0"),
            ([[
                release("0.9007199254740992.0"),
                release("0.9007199254740993.0"),
            ]], "0.9007199254740993.0"),
        ]
        for pages, expected in cases:
            with self.subTest(expected=expected, pages=len(pages)), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                for number, releases in enumerate(pages, 1):
                    (root / f"{number}.json").write_text(json.dumps(releases))
                (root / "empty.json").write_text("[]")
                index_temp = root / "index-temp"
                index_temp.mkdir()
                curl = root / "curl"
                curl.write_text('''#!/bin/sh
set -eu
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift ;;
    https://api.github.com/repos/no-phux/phux/releases?*) url="$1" ;;
  esac
  shift
done
page="${url##*&page=}"
printf '%s\\n' "$page" >> "$FIXTURE/calls"
fixture="$FIXTURE/$page.json"
[ -f "$fixture" ] || fixture="$FIXTURE/empty.json"
cp "$fixture" "$out"
''')
                curl.chmod(0o755)
                env = {
                    **os.environ, "FIXTURE": str(root), "TMPDIR": str(index_temp),
                    "PATH": f"{root}:{os.environ['PATH']}",
                }
                result = run(
                    SHELL, str(INSTALLER), "--dry-run",
                    "--applications-dir", str(root / "Applications"),
                    "--bin-dir", str(root / "launchers"), env=env,
                )
                self.assertIn(f"tag: desktop-v{expected}\n", result.stdout)
                self.assertIn(f"/desktop-v{expected}/phux-desktop-{expected}-macos-arm64.zip", result.stdout)
                self.assertEqual(
                    (root / "calls").read_text().splitlines(),
                    [str(number) for number in range(1, len(pages) + 2)],
                )
                self.assertEqual(list(index_temp.iterdir()), [])
                self.assertFalse((root / "Applications").exists())
                self.assertFalse((root / "launchers").exists())


@unittest.skipUnless(platform.system() == "Darwin" and platform.machine() == "arm64", "macOS arm64 app installer")
class DesktopInstaller(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="phux-desktop-install-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.fixture = self.root / "fixture"
        self.fixture.mkdir()
        self.apps = self.root / "Applications with spaces"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.app = self.root / "source/Phux.app"
        macos = self.app / "Contents/MacOS"
        macos.mkdir(parents=True)
        source = self.root / "main.c"
        source.write_text("int main(void) { return 0; }\n")
        run("cc", str(source), "-o", str(macos / "phux-desktop"))
        self.plist = {
            "CFBundleIdentifier": "dev.phux.desktop",
            "CFBundleExecutable": "phux-desktop",
            "CFBundlePackageType": "APPL",
            "CFBundleVersion": "0.1.0",
            "PhuxDesktopVersion": VERSION,
        }
        self.pack()
        self.script("curl", '''#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    https:*) url="$1"; shift ;;
    *) shift ;;
  esac
done
case "$url" in
  *SHA256SUMS) cp "$FIXTURE/SHA256SUMS" "$out" ;;
  *.zip) cp "$FIXTURE/asset.zip" "$out" ;;
  https://phux.sh/install) cp "$FIXTURE/cli-installer.sh" "$out" ;;
  *) echo "unexpected URL: $url" >&2; exit 1 ;;
esac
''')
        home = self.root / "home"
        self.cli = home / ".local/bin/phux"
        self.cli.parent.mkdir(parents=True)
        self.cli.symlink_to("/usr/bin/true")
        (self.fixture / "cli-installer.sh").write_text(
            'mkdir -p "$PHUX_INSTALL_DIR" && ln -s /usr/bin/true "$PHUX_INSTALL_DIR/phux"\n'
        )
        self.env = {**os.environ, "HOME": str(home), "PATH": f"{self.bin}:/usr/bin:/bin:/usr/sbin:/sbin", "FIXTURE": str(self.fixture)}
        self.env.pop("PHUX_BIN", None)
        self.env.pop("PHUX_DESKTOP_APPLICATIONS_DIR", None)
        self.env.pop("PHUX_DESKTOP_BIN_DIR", None)

    def script(self, name, content):
        path = self.bin / name
        path.write_text(content)
        path.chmod(0o755)

    def pack(self):
        (self.app / "Contents/Info.plist").write_bytes(plistlib.dumps(self.plist))
        run("codesign", "--force", "--sign", "-", str(self.app))
        archive = self.fixture / "asset.zip"
        archive.unlink(missing_ok=True)
        run("ditto", "-c", "-k", "--keepParent", "--norsrc", str(self.app), str(archive))
        self.checksum()

    def checksum(self):
        digest = hashlib.sha256((self.fixture / "asset.zip").read_bytes()).hexdigest()
        (self.fixture / "SHA256SUMS").write_text(f"{digest}  {ASSET}\n")

    def install(self, success=True):
        result = subprocess.run(
            [SHELL, str(INSTALLER), "--version", VERSION, "--applications-dir", str(self.apps), "--bin-dir", str(self.root / "launchers")],
            env=self.env, capture_output=True, text=True,
        )
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    @property
    def installed(self):
        return self.apps / "Phux.app"

    def test_install_and_update_preserve_cli_and_state(self):
        state = self.root / "state/layout.json"
        state.parent.mkdir()
        state.write_text('{"session":"running"}')
        self.env["XDG_STATE_HOME"] = str(state.parent)
        self.install()
        run("codesign", "--verify", "--deep", "--strict", str(self.installed))
        self.plist["TestBuild"] = "updated"
        self.pack()
        self.install()
        result = plistlib.loads((self.installed / "Contents/Info.plist").read_bytes())
        self.assertEqual(result["TestBuild"], "updated")
        self.assertEqual(state.read_text(), '{"session":"running"}')
        launcher = self.root / "launchers/phux-desktop"
        self.assertTrue(os.access(launcher, os.X_OK))
        self.assertFalse((self.root / "launchers/phux").exists())

    def test_missing_cli_is_installed_first(self):
        self.cli.unlink()
        result = self.install()
        self.assertIn("installing it first", result.stdout)
        self.assertTrue(os.access(self.cli, os.X_OK))
        self.assertTrue(self.installed.is_dir())

    def test_failed_cli_install_installs_no_app(self):
        self.cli.unlink()
        (self.fixture / "cli-installer.sh").write_text("exit 1\n")
        result = self.install(success=False)
        self.assertIn("could not install the phux CLI", result.stderr)
        self.assertFalse(self.installed.exists())

    def test_bad_checksum_keeps_existing_install(self):
        self.install()
        before = (self.installed / "Contents/Info.plist").read_bytes()
        (self.fixture / "SHA256SUMS").write_text(f"{'0' * 64}  {ASSET}\n")
        self.install(success=False)
        self.assertEqual((self.installed / "Contents/Info.plist").read_bytes(), before)

    def test_version_mismatch_is_rejected_before_install(self):
        self.plist["PhuxDesktopVersion"] = "0.1.1"
        self.pack()
        result = self.install(success=False)
        self.assertIn("version does not match", result.stderr)
        self.assertFalse(self.installed.exists())

    def test_symlink_archive_is_rejected(self):
        with zipfile.ZipFile(self.fixture / "asset.zip", "a") as archive:
            link = zipfile.ZipInfo("Phux.app/escape")
            link.create_system = 3
            link.external_attr = 0o120777 << 16
            archive.writestr(link, "../../outside")
        self.checksum()
        result = self.install(success=False)
        self.assertIn("symlinks", result.stderr)
        self.assertFalse(self.installed.exists())

    def test_publish_failure_restores_previous_app(self):
        self.install()
        before = (self.installed / "Contents/Info.plist").read_bytes()
        self.plist["TestBuild"] = "must-not-publish"
        self.pack()
        self.script("mv", '''#!/bin/sh
case "$1" in */.phux-desktop-install.*/Phux.app) exit 1 ;; esac
exec /bin/mv "$@"
''')
        self.install(success=False)
        self.assertEqual((self.installed / "Contents/Info.plist").read_bytes(), before)
        self.assertEqual(list(self.apps.glob(".phux-desktop-install.*")), [self.apps / ".phux-desktop-install.lock"])

    def test_unrelated_app_is_never_replaced(self):
        self.installed.mkdir(parents=True)
        sentinel = self.installed / "not-phux"
        sentinel.write_text("keep me")
        result = self.install(success=False)
        self.assertIn("unrelated Phux.app", result.stderr)
        self.assertEqual(sentinel.read_text(), "keep me")

    def test_dead_installer_lock_does_not_block_update(self):
        self.install()
        lock = self.apps / ".phux-desktop-install.lock"
        owner = subprocess.Popen([
            sys.executable, "-c",
            "import fcntl, os, signal, sys; "
            "f = open(sys.argv[1], 'w'); fcntl.flock(f, fcntl.LOCK_EX); "
            "os.kill(os.getpid(), signal.SIGKILL)",
            str(lock),
        ])
        self.assertEqual(owner.wait(), -9)
        self.plist["TestBuild"] = "recovered"
        self.pack()
        self.install()
        installed = plistlib.loads((self.installed / "Contents/Info.plist").read_bytes())
        self.assertEqual(installed["TestBuild"], "recovered")

    def test_live_installer_lock_preserves_existing_app(self):
        self.install()
        before = (self.installed / "Contents/Info.plist").read_bytes()
        lock = self.apps / ".phux-desktop-install.lock"
        with lock.open("w") as owner:
            fcntl.flock(owner, fcntl.LOCK_EX)
            result = self.install(success=False)
        self.assertIn("already publishing", result.stderr)
        self.assertEqual((self.installed / "Contents/Info.plist").read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
