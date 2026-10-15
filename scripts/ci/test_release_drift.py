#!/usr/bin/env python3
"""Run the release sentinel against disposable GitHub responses, never the network."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class ReleaseDriftTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="phux-release-drift-")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        config = json.loads((ROOT / "release-please-config.json").read_text())
        manifest = json.loads((ROOT / ".release-please-manifest.json").read_text())
        self.releases = []
        for path, version in manifest.items():
            component = config["packages"][path].get("component")
            tag = f"{component}-v{version}" if component else f"v{version}"
            self.releases.append(self.release(tag, False, version))
        self.write_binary("git", "#!/bin/sh\necho 2020-01-01T00:00:00Z\n")
        self.write_binary("gh", """#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
if sys.argv[2].endswith('/releases'):
    for row in json.loads(Path(os.environ['RELEASE_FIXTURE']).read_text()):
        print(json.dumps(row))
elif '/issues?' not in sys.argv[2]:
    raise SystemExit('unexpected GitHub call: ' + repr(sys.argv))
""")

    def write_binary(self, name, content):
        binary = self.directory / name
        binary.write_text(content)
        binary.chmod(0o755)

    def release(self, tag, draft, version="0.1.0"):
        return {"tag": tag, "draft": draft, "prerelease": False, "assets": 2,
                "assetNames": [f"phux-desktop-{version}-macos-arm64.zip", "SHA256SUMS"],
                "created": "2020-01-01T00:00:00Z"}

    def check_drift(self):
        fixture = self.directory / "releases.json"
        fixture.write_text(json.dumps(self.releases))
        return subprocess.run(
            ["node", str(ROOT / "scripts/check-release-drift.mjs")], cwd=ROOT,
            env={**os.environ, "PATH": f"{self.directory}:{os.environ['PATH']}",
                 "GITHUB_REPOSITORY": "example/disposable", "GITHUB_ACTIONS": "true",
                 "GRACE_MINUTES": "0", "RELEASE_FIXTURE": str(fixture)},
            text=True, capture_output=True, timeout=15,
        )

    def test_retired_drafts_and_assetless_releases_are_not_repair_candidates(self):
        self.releases.extend([
            self.release("cockpit-v0.28.0", True),
            {**self.release("cockpit-v0.16.1", False), "assets": 0, "assetNames": []},
        ])
        result = self.check_drift()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_surviving_draft_still_reports_its_publication_recovery(self):
        self.releases.append(self.release("desktop-v99.0.0", True))
        result = self.check_drift()
        self.assertEqual(result.returncode, 1)
        self.assertIn("gh workflow run publish.yml --repo example/disposable -f tag=desktop-v99.0.0", result.stderr)

    def test_manifest_still_requires_active_component_release(self):
        missing = self.releases.pop()
        result = self.check_drift()
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{missing['tag']} has no GitHub release", result.stderr)

    def test_active_assetless_publish_is_still_a_failure(self):
        self.releases.append({**self.release("v99.0.0", False), "assets": 0})
        result = self.check_drift()
        self.assertEqual(result.returncode, 1)
        self.assertIn("v99.0.0 is published with zero assets", result.stderr)


if __name__ == "__main__":
    unittest.main()
