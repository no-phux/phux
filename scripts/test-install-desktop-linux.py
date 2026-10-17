#!/usr/bin/env python3
"""Portable Linux installer transactions, using real archives and a fake network."""
import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SHELL = shutil.which("dash") or "/bin/sh"


class LinuxDesktopInstaller(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="phux-linux-installer-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.apps = self.root / "data with spaces"
        self.launchers = self.root / "launchers"
        self.launchers.mkdir()
        (self.launchers / "phux").symlink_to("/usr/bin/true")
        (self.bin / "curl").write_text('''#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in -o) out="$2"; shift ;; https:*) url="$1" ;; esac
  shift
done
printf '%s\\n' "$url" >> "$FIXTURE/calls"
case "$url" in
  */channels/*.json) file=manifest.json ;;
  *SHA256SUMS | *.sha256) file=checksum ;;
  *.minisig) file=signature ;;
  *.tar.gz) file=archive.tar.gz ;;
  *) exit 22 ;;
esac
cp "$FIXTURE/$file" "$out"
''')
        (self.bin / "curl").chmod(0o755)
        self.env = {"PATH": f"{self.bin}:/usr/bin:/bin", "HOME": str(self.root / "home"),
                    "FIXTURE": str(self.root), "XDG_DATA_HOME": str(self.apps)}
        self.target = "x86_64-unknown-linux-gnu"
        self.metadata = {"identifier": "dev.phux.desktop", "version": "1.2.3",
                         "sha": "a" * 40, "channel": "stable", "target": self.target}
        self.asset = "phux-desktop-1.2.3-linux-x64.tar.gz"
        self.pack()

    @property
    def installed(self):
        return self.apps / "phux-desktop"

    def pack(self, extra=None, executable_is_dir=False):
        entries = {"phux-desktop/bin/phux-desktop": b'#!/bin/sh\nprintf "linux:%s\\n" "$*"\n',
                   "phux-desktop/share/phux-desktop/release.json":
                       (json.dumps(self.metadata, indent=2) + "\n").encode()}
        with tarfile.open(self.root / "archive.tar.gz", "w:gz") as tar:
            for name, content in entries.items():
                info = tarfile.TarInfo(name)
                if executable_is_dir and name.endswith("/bin/phux-desktop"):
                    info.type = tarfile.DIRTYPE
                    info.mode = 0o755
                    tar.addfile(info)
                    continue
                info.size = len(content)
                info.mode = 0o755 if name.endswith("/bin/phux-desktop") else 0o644
                tar.addfile(info, io.BytesIO(content))
            if extra:
                tar.addfile(extra)
        self.digest = hashlib.sha256((self.root / "archive.tar.gz").read_bytes()).hexdigest()
        (self.root / "checksum").write_text(f"{self.digest}  {self.asset}\n")

    def install(self, *args, ok=True):
        result = subprocess.run([SHELL, str(ROOT / "scripts/install.sh"), "--desktop",
                                 "--os", "linux", "--arch", "x86_64", "--install-dir",
                                 str(self.launchers), *(args or ("--version", "1.2.3"))],
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, ok, result.stdout + result.stderr)
        return result

    def test_install_update_launcher_and_default_xdg_destination(self):
        self.install()
        result = subprocess.run([str(self.launchers / "phux-desktop"), "argument with spaces"],
                                capture_output=True, text=True, check=True)
        self.assertEqual(result.stdout, "linux:argument with spaces\n")
        self.metadata["sha"] = "b" * 40
        self.pack()
        self.install()
        metadata = json.loads((self.installed / "share/phux-desktop/release.json").read_text())
        self.assertEqual(metadata["sha"], "b" * 40)
        self.assertFalse(list(self.apps.glob(".phux-desktop*")))

    def test_both_architectures_and_next_names(self):
        self.metadata.update(channel="next", target="aarch64-unknown-linux-gnu")
        self.asset = f"phux-desktop-next.{'a' * 40}-linux-arm64.tar.gz"
        self.pack()
        result = self.install("--arch", "arm64", "--channel", "next", "--revision", "a" * 40)
        self.assertIn("installed Phux", result.stderr)
        self.assertIn(self.asset, (self.root / "calls").read_text())

    def test_manifest_resolution(self):
        manifest = {"schema_version": 1, "channel": "stable", "products": {
            "desktop": {"version": "1.2.3", "sha": "a" * 40, "protocol": "0.9",
                        "artifacts": {self.target: {"sha256": self.digest}}}}}
        (self.root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        self.install("--channel", "stable")
        self.assertTrue(self.installed.is_dir())

    def test_checksum_and_required_signature_refuse_without_replacing(self):
        self.install()
        before = (self.installed / "share/phux-desktop/release.json").read_bytes()
        result = self.install("--version", "1.2.3", "--sha256", "0" * 64, ok=False)
        self.assertIn("checksum mismatch", result.stderr)
        self.env["PHUX_REQUIRE_SIGNATURE"] = "1"
        result = self.install(ok=False)
        self.assertTrue("minisign" in result.stderr or "signature" in result.stderr, result.stderr)
        self.assertEqual((self.installed / "share/phux-desktop/release.json").read_bytes(), before)

    def test_invalid_signature_is_refused_even_with_an_installed_cli(self):
        self.install()
        before = (self.installed / "share/phux-desktop/release.json").read_bytes()
        (self.bin / "minisign").write_text("#!/bin/sh\nexit 1\n")
        (self.bin / "minisign").chmod(0o755)
        (self.root / "signature").write_text("foreign signature")
        self.env["PHUX_REQUIRE_SIGNATURE"] = "1"
        self.install(ok=False)
        self.assertEqual((self.installed / "share/phux-desktop/release.json").read_bytes(), before)

    def test_directory_is_not_a_desktop_executable(self):
        self.install()
        before = (self.installed / "bin/phux-desktop").read_bytes()
        self.pack(executable_is_dir=True)
        self.assertIn("no desktop executable", self.install(ok=False).stderr)
        self.assertEqual((self.installed / "bin/phux-desktop").read_bytes(), before)

    def test_custom_cli_on_path_is_not_reinstalled(self):
        (self.launchers / "phux").unlink()
        (self.bin / "phux").symlink_to("/usr/bin/true")
        self.install()
        self.assertFalse((self.launchers / "phux").exists())

    def test_identity_version_channel_and_target_are_checked(self):
        for field, value in (("identifier", "dev.example.other"), ("version", "9.9.9"),
                             ("target", "aarch64-unknown-linux-gnu"), ("channel", "next")):
            with self.subTest(field=field):
                original = self.metadata[field]
                self.metadata[field] = value
                self.pack()
                self.install(ok=False)
                self.assertFalse(self.installed.exists())
                self.metadata[field] = original

    def test_links_traversal_and_special_files_are_refused_before_extraction(self):
        for kind, name in ((tarfile.SYMTYPE, "phux-desktop/share"),
                           (tarfile.LNKTYPE, "phux-desktop/bin/phux-desktop"),
                           (tarfile.FIFOTYPE, "phux-desktop/share/phux-desktop/NOTICE"),
                           (tarfile.REGTYPE, "phux-desktop/../../escaped")):
            with self.subTest(kind=kind, name=name):
                info = tarfile.TarInfo(name)
                info.type = kind
                info.linkname = str(self.root)
                self.pack(info)
                self.install(ok=False)
                self.assertFalse(self.installed.exists())
                self.assertFalse((self.root / "escaped").exists())

    def test_lock_and_unrelated_directory_are_preserved(self):
        self.apps.mkdir()
        lock = self.apps / ".phux-desktop-install.lock"
        lock.mkdir()
        self.assertIn("another Desktop install", self.install(ok=False).stderr)
        self.assertTrue(lock.is_dir())
        lock.rmdir()
        self.installed.mkdir()
        (self.installed / "keep").write_text("unrelated")
        self.assertIn("unrelated", self.install(ok=False).stderr)
        self.assertEqual((self.installed / "keep").read_text(), "unrelated")

    def test_failed_publish_restores_previous_bundle(self):
        self.install()
        original = (self.installed / "share/phux-desktop/release.json").read_bytes()
        self.metadata["sha"] = "b" * 40
        self.pack()
        (self.bin / "mv").write_text('''#!/bin/sh
case "$1" in */.phux-desktop-install.*/phux-desktop) exit 1 ;; esac
exec /bin/mv "$@"
''')
        (self.bin / "mv").chmod(0o755)
        self.install(ok=False)
        self.assertEqual((self.installed / "share/phux-desktop/release.json").read_bytes(), original)
        self.assertFalse(list(self.apps.glob(".phux-desktop*")))

    def test_staging_is_explicitly_unsupported(self):
        result = self.install("--version", "1.2.3", "--stage-dir", str(self.root / "stage"), ok=False)
        self.assertIn("not supported on Linux", result.stderr)
        self.assertFalse(self.installed.exists())

    def test_without_xdg_data_home_uses_user_local_share(self):
        self.env.pop("XDG_DATA_HOME")
        self.install()
        self.assertTrue((self.root / "home/.local/share/phux-desktop/bin/phux-desktop").is_file())

    def test_invalid_xdg_data_home_uses_user_local_share(self):
        for value in ("", "relative/data"):
            with self.subTest(value=value):
                self.env["XDG_DATA_HOME"] = value
                self.install()
                self.assertTrue((self.root / "home/.local/share/phux-desktop/bin/phux-desktop").is_file())

    @unittest.skipUnless(shutil.which("bun"), "packaging requires Bun")
    def test_actual_packager_bundle_is_accepted(self):
        binary = self.root / "elf"
        header = bytearray(20)
        header[:6] = b"\x7fELF\x02\x01"
        header[18] = 62
        binary.write_bytes(header)
        options = {"binary": str(binary), "output": str(self.root / "packaged"),
                   "version": "1.2.3", "sha": "a" * 40, "target": self.target,
                   "channel": "stable", "notices": "fixture"}
        script = ROOT / "clients/desktop/scripts/package-linux.ts"
        subprocess.run([shutil.which("bun"), "-e",
                        f"import {{createLinuxBundle}} from {json.dumps(str(script))};"
                        f"createLinuxBundle({json.dumps(options)});"], check=True)
        subprocess.run(["tar", "-czf", str(self.root / "archive.tar.gz"), "-C",
                        str(self.root / "packaged"), "phux-desktop"], check=True)
        digest = hashlib.sha256((self.root / "archive.tar.gz").read_bytes()).hexdigest()
        self.install("--version", "1.2.3", "--sha256", digest)
        self.assertTrue((self.installed / "share/phux-desktop/LICENSE").is_file())
        entry = (self.apps / "applications/dev.phux.desktop.desktop").read_text()
        executable = str(self.installed.resolve() / "bin/phux-desktop")
        quoted = "".join("\\" + char if char in '\\"`$' else char for char in executable)
        quoted = quoted.replace("\\", "\\\\").replace("%", "%%")
        self.assertIn(f'Exec="{quoted}"', entry)
        icon = str(self.installed.resolve() / "share/icons/").replace("\\", "\\\\")
        self.assertIn(f"Icon={icon}", entry)

    def test_desktop_entry_escapes_exec_field_codes_and_shell_characters(self):
        self.apps = self.root / 'data "$HOME`test\\percent%f'
        self.env["XDG_DATA_HOME"] = str(self.apps)
        self.test_actual_packager_bundle_is_accepted()


if __name__ == "__main__":
    unittest.main()
