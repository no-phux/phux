"""Runner isolation/dispatch tests use header-only fake Cargo, never GPU evidence."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


class VisualRunner(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_runner_rejects_relative_and_existing_paths_before_cargo(self):
        script = Path(__file__).with_name("visual-review.sh")
        for out in ("relative", str(self.root)):
            with self.subTest(out=out):
                result = subprocess.run(["bash", str(script), out], capture_output=True, text=True)
                self.assertEqual(result.returncode, 2)
        self.assertFalse((self.root / "logs").exists())

    def test_runner_uses_serial_release_tests_sanitizes_env_and_packages(self):
        repo = self.root / "repo"
        scripts = repo / "clients/rune/scripts"
        scripts.mkdir(parents=True)
        for name in ("visual-review.sh", "visual_review.py", "visual_motion.py",
                     "visual_navigation.py", "visual_palette.py", "visual_states.py", "visual_themes.py", "visual_acp.py"):
            shutil.copyfile(Path(__file__).with_name(name), scripts / name)
        (scripts.parent / "Cargo.toml").write_text('rune = { rev = "test-pin" }\n')
        for args in (["init", "-q"], ["add", "."],
                     ["-c", "core.hooksPath=/dev/null", "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                      "-c", "commit.gpgsign=false", "commit", "-qm", "fixture"]):
            subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)
        binaries = self.root / "bin"
        binaries.mkdir()
        cargo = binaries / "cargo"
        fixture = Path(__file__).with_name("visual_runner_fixture.py").read_text()
        cargo.write_text(f"#!{sys.executable}\n{fixture}")
        cargo.chmod(0o755)
        out = self.root / "fresh review"
        private_seed = self.root / "inherited-private-prefs"
        seed = private_seed / "sh.phux.rune/prefs.json"
        seed.parent.mkdir(parents=True)
        seed_bytes = b'{"font_size":15,"tour_offered":false}\n'
        seed.write_bytes(seed_bytes)
        environment = {
            **os.environ, "PATH": str(binaries) + os.pathsep + os.environ["PATH"],
            "PHUX_SOCKET": "do-not-connect", "PHUX_WS_TOKENS": "do-not-use",
            "SIMCTL_CHILD_PHUX_PROFILE": "default",
            "RUNE_PREFS_DIR": str(private_seed),
        }
        result = subprocess.run(["bash", str(scripts / "visual-review.sh"), str(out)],
                                env=environment, cwd=self.root, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        manifest = json.loads((out / "manifest.json").read_text())
        self.assertEqual(len(manifest["cases"]), 2989)
        self.assertEqual(manifest["currentRunePin"], "test-pin")
        self.assertFalse(manifest["dirty"])
        self.assertEqual(len(list((out / "logs").glob("*.log"))), 32)
        self.assertEqual(seed.read_bytes(), seed_bytes)
        self.assertEqual({path.relative_to(private_seed).as_posix(): path.read_bytes()
                          for path in private_seed.rglob("*") if path.is_file()},
                         {"sh.phux.rune/prefs.json": seed_bytes})
        self.assertEqual(json.loads((out / "prefs/runner-fixture.json").read_text()),
                         {"fixture": True})


if __name__ == "__main__":
    unittest.main()
