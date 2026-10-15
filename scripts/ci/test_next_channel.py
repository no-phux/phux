#!/usr/bin/env python3
"""The remaining next lanes retain independent CLI and GPUI publication."""

import subprocess
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]


class NextChannelTests(unittest.TestCase):
    def test_retired_publisher_option_is_rejected_before_publication(self):
        result = subprocess.run(
            ["bash", "scripts/publish-next-channel.sh", "--cockpit", "a" * 40, "missing"],
            cwd=ROOT, text=True, capture_output=True, timeout=10,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("SHA DIST_DIR", result.stderr)

    def test_next_workflow_has_only_surviving_product_lanes(self):
        workflow = (ROOT / ".github/workflows/next-release.yml").read_text()
        self.assertNotIn("cockpit", workflow.lower())
        self.assertIn("needs: [gate, build, desktop]", workflow)
        self.assertIn("needs.build.result == 'success' || needs.desktop.result == 'success'", workflow)
        self.assertIn("uses: ./.github/workflows/desktop-release.yml", workflow)
        self.assertIn('run: bash scripts/publish-next-channel.sh "$SHA" dist', workflow)
        self.assertIn('gh release upload next dist/desktop-channel.json --clobber', workflow)


if __name__ == "__main__":
    unittest.main()
