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
                    # The Next qualification job exports PHUX_CHANNEL=next;
                    # this fixture specifically exercises the stable default.
                    "PHUX_CHANNEL": "",
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


class DesktopPublication(unittest.TestCase):
    """Run the workflow's actual publication shell against an inert gh mock."""

    def publish(self, existing=True, corrupt=False, fail_pointer=False, fail_previous=False, already_current=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dist = root / "dist"
            dist.mkdir()
            sha = "a" * 40
            previous = "b" * 40
            archive = f"phux-desktop-next.{sha}-macos-arm64.zip"
            (dist / archive).write_bytes(b"qualified archive")
            digest = hashlib.sha256((dist / archive).read_bytes()).hexdigest()
            (dist / f"{archive}.sha256").write_text(f"{digest}  {archive}\n")
            (dist / "desktop-channel.json").write_text(json.dumps({
                "schema_version": 1, "channel": "next", "product": "desktop",
                "sha": sha, "version": VERSION, "archive": archive,
                "sha256": "0" * 64 if corrupt else digest,
            }))
            names = [archive, f"{archive}.sha256",
                     f"phux-desktop-next.{previous}-macos-arm64.zip",
                     f"phux-desktop-next.{previous}-macos-arm64.zip.sha256",
                     f"phux-desktop-next.{'c' * 40}-macos-arm64.zip",
                     "phux-next.cli.tar.gz", "phux-cockpit-next.cockpit.zip",
                     "channel.json", "cockpit-channel.json", "desktop-channel.json"]
            (root / "names.json").write_text(json.dumps(names))
            mock = root / "gh"
            mock.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['MOCK_ROOT'])
args = sys.argv[1:]
with (root / 'calls.jsonl').open('a') as log:
    log.write(json.dumps(args) + '\\n')
if args[:3] == ['release', 'view', 'next']:
    if os.environ['MOCK_EXISTING'] == '0' and not (root / 'created').exists():
        sys.exit(1)
    if '--json' in args:
        print('\\n'.join(json.loads((root / 'names.json').read_text())))
elif args[:2] == ['release', 'download']:
    if os.environ['MOCK_FAIL_PREVIOUS'] == '1':
        sys.exit(1)
    directory = pathlib.Path(args[args.index('--dir') + 1])
    previous = 'a' if os.environ['MOCK_ALREADY_CURRENT'] == '1' else 'b'
    (directory / 'desktop-channel.json').write_text(json.dumps({'sha': previous * 40}))
elif args[:2] == ['release', 'upload']:
    if 'dist/desktop-channel.json' in args and os.environ['MOCK_FAIL_POINTER'] == '1':
        sys.exit(1)
elif args[:2] == ['release', 'create']:
    (root / 'created').touch()
''')
            mock.chmod(0o755)
            workflow = (ROOT / ".github/workflows/next-release.yml").read_text()
            section = workflow.split("      - name: Publish Phux Desktop onto the next prerelease\n", 1)[1]
            script = section.split("        run: |\n", 1)[1]
            script = "\n".join(line[10:] for line in script.splitlines()) + "\n"
            result = subprocess.run(["bash", "-c", script], cwd=root, capture_output=True, text=True,
                                    env={**os.environ, "PATH": f"{root}:{os.environ['PATH']}",
                                         "SHA": sha, "GH_REPO": "test/phux", "MOCK_ROOT": str(root),
                                          "MOCK_EXISTING": str(int(existing)), "MOCK_FAIL_POINTER": str(int(fail_pointer)),
                                          "MOCK_FAIL_PREVIOUS": str(int(fail_previous)), "MOCK_ALREADY_CURRENT": str(int(already_current))})
            calls = root / "calls.jsonl"
            return result, [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []

    def test_existing_next_uploads_pointer_last_and_prunes_only_old_desktop(self):
        result, calls = self.publish()
        self.assertEqual(result.returncode, 0, result.stderr)
        uploads = [call for call in calls if call[:2] == ["release", "upload"]]
        self.assertEqual(len(uploads), 2)
        self.assertTrue(uploads[0][3].endswith(".zip"))
        self.assertEqual(uploads[1][3], "dist/desktop-channel.json")
        deleted = [call[3] for call in calls if call[:2] == ["release", "delete-asset"]]
        self.assertEqual(deleted, [f"phux-desktop-next.{'c' * 40}-macos-arm64.zip"])

    def test_desktop_can_create_next_without_cli_or_cockpit(self):
        result, calls = self.publish(existing=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(any(call[:2] == ["release", "create"] for call in calls))

    def test_corrupt_metadata_never_publishes(self):
        result, calls = self.publish(corrupt=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])

    def test_pointer_failure_never_prunes_previous_assets(self):
        result, calls = self.publish(fail_pointer=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(call[:2] == ["release", "delete-asset"] for call in calls))

    def test_unreadable_previous_pointer_and_retries_preserve_pinned_assets(self):
        for options in ({"fail_previous": True}, {"already_current": True}):
            with self.subTest(options=options):
                result, calls = self.publish(**options)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertFalse(any(call[:2] == ["release", "delete-asset"] for call in calls))

    def test_other_product_publication_failure_cannot_skip_desktop(self):
        workflow = (ROOT / ".github/workflows/next-release.yml").read_text()
        desktop = workflow.split("      - name: Publish Phux Desktop onto the next prerelease\n", 1)[1]
        self.assertIn("if: ${{ !cancelled() && needs.desktop.result == 'success' }}", desktop)

    def test_gate_uses_desktop_pointer_and_skips_docs_only_changes(self):
        workflow = (ROOT / ".github/workflows/next-release.yml").read_text()
        section = workflow.split("      - name: Decide whether to build\n", 1)[1].split("\n  build:", 1)[0]
        script = section.split("        run: |\n", 1)[1]
        script = "\n".join(line[10:] for line in script.splitlines()) + "\n"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            mocks = {
                "curl": '''#!/bin/sh
case "$2" in
  */desktop-channel.json) sha=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb ;;
  *) sha=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa ;;
esac
printf '{"sha":"%s"}\\n' "$sha"
''',
                "git": '''#!/usr/bin/env python3
import os, sys
args = sys.argv[1:]
if args[0] == 'diff':
    paths = args[args.index('--') + 1:]
    changed = os.environ['MOCK_CHANGED']
    if not paths or any(changed == path or changed.startswith(path + '/') for path in paths):
        print(changed)
''',
            }
            for name, content in mocks.items():
                mock = root / name
                mock.write_text(content)
                mock.chmod(0o755)
            for changed, expected in (("clients/desktop/native/src/main.rs", "true"),
                                      ("scripts/install-desktop.sh", "true"),
                                      ("crates/phux-client/src/lib.rs", "true"),
                                      ("docs/RELEASING.md", "false")):
                with self.subTest(changed=changed):
                    output = root / "output"
                    output.unlink(missing_ok=True)
                    result = subprocess.run(["bash", "-c", script], cwd=ROOT, capture_output=True, text=True,
                                            env={**os.environ, "PATH": f"{root}:{os.environ['PATH']}",
                                                 "SHA": "a" * 40, "GITHUB_OUTPUT": str(output),
                                                 "GITHUB_REPOSITORY": "test/phux", "MOCK_CHANGED": changed})
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn(f"desktop_build={expected}\n", output.read_text())
                    self.assertIn("should_build=false\n", output.read_text())
                    self.assertIn("cockpit_build=false\n", output.read_text())


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
        self.asset = ASSET
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
  *SHA256SUMS|*.sha256) cp "$FIXTURE/SHA256SUMS" "$out" ;;
  *desktop-channel.json) cp "$FIXTURE/desktop-channel.json" "$out" ;;
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
        self.env.pop("PHUX_CHANNEL", None)

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
        (self.fixture / "SHA256SUMS").write_text(f"{digest}  {self.asset}\n")

    def install(self, success=True, args=None):
        result = subprocess.run(
            [SHELL, str(INSTALLER), *(args if args is not None else ["--version", VERSION]), "--applications-dir", str(self.apps), "--bin-dir", str(self.root / "launchers")],
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

    def next_fixture(self):
        sha = "a" * 40
        self.asset = f"phux-desktop-next.{sha}-macos-arm64.zip"
        self.plist["PhuxBuildSHA"] = sha
        self.plist["PhuxDesktopChannel"] = "next"
        self.pack()
        (self.fixture / "desktop-channel.json").write_text(json.dumps({
            "schema_version": 1, "channel": "next", "product": "desktop",
            "sha": sha, "version": VERSION,
        }))
        return sha

    def test_next_pointer_selects_desktop_sha_and_checks_bundle_revision(self):
        sha = self.next_fixture()
        result = self.install(args=["--channel", "next"])
        self.assertIn(f"phux: ok: installed Phux next.{sha}", result.stderr)
        self.assertNotIn("installed Phux", result.stdout)
        self.plist["PhuxBuildSHA"] = "b" * 40
        self.pack()
        result = self.install(success=False, args=["--channel", "next"])
        self.assertIn("revision does not match", result.stderr)
        installed = plistlib.loads((self.installed / "Contents/Info.plist").read_bytes())
        self.assertEqual(installed["PhuxBuildSHA"], sha)

    def test_revision_pin_never_fetches_moving_pointer(self):
        sha = self.next_fixture()
        (self.fixture / "desktop-channel.json").unlink()
        self.install(args=["--channel", "next", "--revision", sha])

    def test_next_rejects_bundle_packaged_as_stable(self):
        sha = self.next_fixture()
        self.plist["PhuxDesktopChannel"] = "stable"
        self.pack()
        result = self.install(success=False, args=["--channel", "next", "--revision", sha])
        self.assertIn("not a next-channel build", result.stderr)
        self.assertFalse(self.apps.exists())

    def test_stable_rejects_bundle_packaged_as_next(self):
        self.plist["PhuxDesktopChannel"] = "next"
        self.pack()
        result = self.install(success=False)
        self.assertIn("not a stable-channel build", result.stderr)
        self.assertFalse(self.apps.exists())

    def test_latest_alias_and_explicit_stable(self):
        for channel in ("latest", "stable"):
            with self.subTest(channel=channel):
                result = self.install(args=["--channel", channel, "--version", VERSION, "--dry-run"])
                self.assertIn("channel: stable\n", result.stdout)
                self.assertIn(f"/desktop-v{VERSION}/{ASSET}", result.stdout)
                self.assertFalse(self.apps.exists())

    def test_invalid_channels_and_revision_combinations(self):
        for args in (
            ["--channel", "alpha"], ["--channel"], ["--revision"],
            ["--channel", "next", "--revision", "bad"],
            ["--channel", "stable", "--revision", "a" * 40],
            ["--channel", "next", "--revision", "a" * 40, "--version", VERSION],
            ["--channel", "next", "--version", VERSION],
        ):
            with self.subTest(args=args):
                self.install(success=False, args=args)
                self.assertFalse(self.apps.exists())

    def test_invalid_next_pointer_never_installs(self):
        self.next_fixture()
        pointer = self.fixture / "desktop-channel.json"
        valid = json.loads(pointer.read_text())
        cases = ['not json', json.dumps({"nested": valid})]
        for key, value in (("product", "phux"), ("channel", "stable"),
                           ("sha", "../evil"), ("version", "bad"), ("schema_version", 2)):
            cases.append(json.dumps({**valid, key: value}))
        for body in cases:
            with self.subTest(body=body):
                pointer.write_text(body)
                self.install(success=False, args=["--channel", "next"])
                self.assertFalse(self.apps.exists())

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
        self.assertIn("phux: info: phux CLI not found; installing it first", result.stderr)
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
