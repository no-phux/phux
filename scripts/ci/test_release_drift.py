#!/usr/bin/env python3
"""The release drift check, against a fake `gh`."""

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
FAKE_GH = """#!/usr/bin/env python3
import json, os, sys
state = json.load(open(os.environ["DRIFT_STATE"]))
if sys.argv[1:3] == ["release", "download"]:
    print(json.dumps(state["stable"]))
elif sys.argv[2].endswith("/releases"):
    for row in state["releases"]:
        print(json.dumps(row))
elif "/issues?" not in sys.argv[2]:
    raise SystemExit("unexpected gh call: " + repr(sys.argv))
"""


class ReleaseDriftTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.bin = Path(scratch.name)
        (self.bin / "gh").write_text(FAKE_GH)
        (self.bin / "git").write_text("#!/bin/sh\necho 2020-01-01T00:00:00Z\n")
        for name in ("gh", "git"):
            (self.bin / name).chmod(0o755)
        config = json.loads((ROOT / "release-please-config.json").read_text())
        manifest = json.loads((ROOT / ".release-please-manifest.json").read_text())
        self.releases = []
        for path, version in manifest.items():
            component = config["packages"][path].get("component")
            self.releases.append(self.release(f"{component}-v{version}" if component else f"v{version}"))
        self.stable = {"products": {"phux": {"version": manifest["."]},
                                    "desktop": {"version": manifest["clients/desktop"]}}}

    @staticmethod
    def release(tag, draft=False, assets=2):
        return {"tag": tag, "draft": draft, "prerelease": False, "assets": assets,
                "created": "2020-01-01T00:00:00Z"}

    def drift(self):
        state = self.bin / "state.json"
        state.write_text(json.dumps({"releases": self.releases, "stable": self.stable}))
        return subprocess.run(
            ["node", str(ROOT / "scripts/check-release-drift.mjs")], cwd=ROOT, text=True,
            capture_output=True, timeout=15,
            env={**os.environ, "PATH": f"{self.bin}:{os.environ['PATH']}", "GITHUB_ACTIONS": "true",
                 "GITHUB_REPOSITORY": "example/repo", "GRACE_MINUTES": "0", "DRIFT_STATE": str(state)})

    def assertStuck(self, message):
        result = self.drift()
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn(message, result.stderr)

    def test_a_finished_pipeline_passes(self):
        result = self.drift()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_an_old_draft_names_its_recovery(self):
        self.releases.append(self.release("desktop-v99.0.0", draft=True))
        self.assertStuck("gh workflow run publish.yml --repo example/repo -f tag=desktop-v99.0.0")

    def test_a_public_release_needs_assets(self):
        self.releases.append(self.release("v0.0.9", assets=0))
        self.assertStuck("v0.0.9 is published with no assets")

    def test_every_manifest_version_needs_a_release(self):
        missing = self.releases.pop()
        self.assertStuck(f"{missing['tag']} has no release")

    def test_the_stable_manifest_names_the_newest_release(self):
        self.releases.append(self.release("v99.0.0"))
        self.assertStuck("but v99.0.0 is published")


if __name__ == "__main__":
    unittest.main()
