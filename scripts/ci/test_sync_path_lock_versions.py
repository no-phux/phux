#!/usr/bin/env python3
"""Version synchronization preserves unrelated packages and detects stale locks."""

from pathlib import Path
import runpy
import tempfile
import unittest

SYNC = runpy.run_path(str(Path(__file__).resolve().parents[1] / "sync-path-lock-versions.py"))


def lock(version):
    return f'''version = 4

[[package]]
name = "phux-protocol"
version = "{version}"

[[package]]
name = "phux-edge"
version = "0.0.0"

[[package]]
name = "registry-package"
version = "0.46.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
'''


class LockSyncTests(unittest.TestCase):
    def test_only_shared_path_versions_change(self):
        before = lock("0.46.0")
        after = SYNC["sync"](before, {"phux-protocol": "0.48.0", "registry-package": "0.48.0"})
        self.assertEqual(after, lock("0.48.0"))
        self.assertEqual(SYNC["sync"](after, {"phux-protocol": "0.48.0"}), after)

    def test_check_reports_without_mutation_and_fix_is_idempotent(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "root.lock"
            edge = Path(temporary) / "edge.lock"
            root.write_text(lock("0.48.0"))
            edge.write_text(lock("0.46.0"))
            args = ["sync", str(root), str(edge)]
            self.assertEqual(SYNC["main"](["sync", "--check", *args[1:]]), 1)
            self.assertEqual(edge.read_text(), lock("0.46.0"))
            self.assertEqual(SYNC["main"](args), 0)
            self.assertEqual(SYNC["main"](["sync", "--check", *args[1:]]), 0)
            self.assertEqual(edge.read_text(), lock("0.48.0"))


if __name__ == "__main__":
    unittest.main()
