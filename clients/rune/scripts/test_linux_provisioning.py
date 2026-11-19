from pathlib import Path
import os
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import configure_ubuntu_archive as archive


class UbuntuArchiveTests(unittest.TestCase):
    def test_deb822_preserves_suites_components_architectures_and_signing_key(self):
        source = (
            "Types: deb deb-src\n"
            "URIs: http://us-east-2.ec2.archive.ubuntu.com/ubuntu/\n"
            "Suites: noble noble-updates noble-backports noble-security\n"
            "Components: main restricted universe multiverse\n"
            "Architectures: amd64\n"
            "Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n"
        )
        self.assertEqual(
            archive.canonical_archive(source),
            source.replace("http://us-east-2.ec2.archive.ubuntu.com/ubuntu/",
                           "https://archive.ubuntu.com/ubuntu/"),
        )

    def test_legacy_and_multiple_uri_fields_keep_their_options(self):
        source = (
            "deb [arch=amd64 signed-by=/keys/ubuntu.gpg] "
            "https://us-east-2.ec2.archive.ubuntu.com/ubuntu noble main\n"
            "deb-src http://us-east-2.ec2.archive.ubuntu.com/ubuntu/ noble universe\n"
            "URIs: http://us-east-2.ec2.archive.ubuntu.com/ubuntu "
            "https://archive.ubuntu.com/ubuntu\n"
        )
        result = archive.canonical_archive(source)
        self.assertNotIn(".ec2.archive.ubuntu.com", result)
        self.assertEqual(result.count("https://archive.ubuntu.com/ubuntu"), 4)
        self.assertIn("[arch=amd64 signed-by=/keys/ubuntu.gpg]", result)
        self.assertIn("deb-src https://archive.ubuntu.com/ubuntu/ noble universe", result)

    def test_arm_ports_other_sources_comments_and_lookalikes_are_unchanged(self):
        source = (
            "URIs: http://ports.ubuntu.com/ubuntu-ports\n"
            "URIs: https://security.ubuntu.com/ubuntu\n"
            "URIs: http://azure.archive.ubuntu.com/ubuntu\n"
            "URIs: http://eu-west-1.ec2.archive.ubuntu.com/ubuntu\n"
            "deb https://packages.microsoft.com/repos/code stable main\n"
            "# deb http://us-east-2.ec2.archive.ubuntu.com/ubuntu noble main\n"
            "deb https://us-east-2.ec2.archive.ubuntu.com.evil/ubuntu noble main\n"
            "deb https://us-east-2.ec2.archive.ubuntu.com/ubuntu-other noble main\n"
            "deb https://user@us-east-2.ec2.archive.ubuntu.com/ubuntu noble main\n"
            "deb https://us-east-2.ec2.archive.ubuntu.com:443/ubuntu noble main\n"
            "deb https://example.com/http://us-east-2.ec2.archive.ubuntu.com/ubuntu noble main\n"
        )
        self.assertEqual(archive.canonical_archive(source), source)

    def test_configuration_handles_both_source_formats_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            includes = root / "sources.list.d"
            includes.mkdir()
            paths = [root / "apt-mirrors.txt", root / "sources.list",
                     includes / "ubuntu.list", includes / "ubuntu.sources"]
            for source in paths:
                source.write_text("URIs: http://us-east-2.ec2.archive.ubuntu.com/ubuntu\n")
            ignored = includes / "ubuntu.sources.save"
            ignored.write_text("http://us-east-2.ec2.archive.ubuntu.com/ubuntu\n")
            archive.configure_sources(root)
            for source in paths:
                self.assertEqual(source.read_text(), "URIs: https://archive.ubuntu.com/ubuntu\n")
            self.assertIn(".ec2.archive.ubuntu.com", ignored.read_text())
            with patch.object(Path, "write_bytes", side_effect=AssertionError("unexpected write")):
                archive.configure_sources(root)

    def test_mirror_file_keeps_priorities_fallbacks_and_source_indirection(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            mirrors = root / "apt-mirrors.txt"
            original = (
                b"# Preferred archives\r\n"
                b"http://us-east-2.ec2.archive.ubuntu.com/ubuntu/\tpriority:1\r\n"
                b"https://archive.ubuntu.com/ubuntu/\tpriority:2\r\n"
                b"https://security.ubuntu.com/ubuntu/\tpriority:3\r\n"
            )
            mirrors.write_bytes(original)
            source_dir = root / "sources.list.d"
            source_dir.mkdir()
            source = source_dir / "ubuntu.sources"
            source_bytes = (
                b"URIs: mirror+file:/etc/apt/apt-mirrors.txt\n"
                b"Suites: noble noble-updates noble-security\n"
                b"Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n"
            )
            source.write_bytes(source_bytes)
            archive.configure_sources(root)
            self.assertEqual(mirrors.read_bytes(), original.replace(
                b"http://us-east-2.ec2.archive.ubuntu.com", b"https://archive.ubuntu.com"))
            self.assertEqual(source.read_bytes(), source_bytes)

    def test_missing_legacy_file_is_normal_but_source_io_failures_are_not_hidden(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive.configure_sources(root)
            (root / "sources.list").write_text("deb http://us-east-2.ec2.archive.ubuntu.com/ubuntu noble main\n")
            with patch.object(Path, "read_bytes", side_effect=OSError("unreadable")):
                with self.assertRaises(OSError):
                    archive.configure_sources(root)
            with patch.object(Path, "write_bytes", side_effect=OSError("unwritable")):
                with self.assertRaises(OSError):
                    archive.configure_sources(root)

    def run_dependency_step(self, configure_status):
        repo = Path(__file__).resolve().parents[3]
        action = (repo / ".github/actions/rune-host/action.yml").read_text()
        step = action.split("    - name: Linux build and qualification dependencies\n", 1)[1]
        step = step.split("    - name: Toolchains\n", 1)[0].split("      run: |\n", 1)[1]
        command = "\n".join(line.removeprefix("        ") for line in step.splitlines())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "sudo").write_text(
                '#!/bin/bash\nprintf "%s\\n" "$*" >> "$CALLS"\n'
                'if [[ "$1" == python3 ]]; then exit "$CONFIGURE_STATUS"; fi\n'
                '[[ "$1" == apt-get ]]\n'
            )
            (root / "find").write_text("#!/bin/bash\necho /fixture/lvp_icd.json\n")
            (root / "sudo").chmod(0o700)
            (root / "find").chmod(0o700)
            result = subprocess.run(
                ["bash", "-euo", "pipefail", "-c", command], capture_output=True, text=True,
                env={**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"],
                     "CALLS": str(root / "calls"), "GITHUB_ENV": str(root / "env"),
                     "CONFIGURE_STATUS": str(configure_status)},
            )
            return result, (root / "calls").read_text().splitlines()

    def test_action_normalizes_before_update_and_keeps_every_install_argument(self):
        result, calls = self.run_dependency_step(0)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, [
            "python3 clients/rune/scripts/configure_ubuntu_archive.py",
            "apt-get update",
            "apt-get install -y build-essential pkg-config mold libxkbcommon-dev libwayland-dev "
            "libxkbcommon-x11-0 mesa-vulkan-drivers xvfb xauth weston "
            "sway wtype wl-clipboard grim jq fcitx5 fcitx5-chinese-addons dbus",
        ])

    def test_action_stops_before_apt_if_normalization_fails(self):
        result, calls = self.run_dependency_step(1)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, ["python3 clients/rune/scripts/configure_ubuntu_archive.py"])


if __name__ == "__main__":
    unittest.main()
