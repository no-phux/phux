"""Exercise the actual subprocess/JSON boundary, without contacting GitHub."""
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import release


class OpenBumpPrTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="release lookup ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.payload = self.root / "response.json"
        self.arguments = self.root / "arguments.json"
        helper = self.root / "fake_gh.py"
        helper.write_text("""
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
Path(os.environ["LOOKUP_ARGS"]).write_text(json.dumps(args))
assert args[:2] == ["pr", "list"], "only the read-only lookup is permitted"
if os.environ.get("LOOKUP_FAIL"):
    sys.exit(7)
rows = json.loads(Path(os.environ["LOOKUP_RESPONSE"]).read_text())
if "-q" in args:
    # gh's --jq prints nothing for null: the old command's actual failure.
    selected = next((row for row in rows if not row["isCrossRepository"]), None)
    if selected is not None:
        print(json.dumps(selected))
else:
    print(json.dumps(rows))
""")
        gh = self.root / "gh"
        gh.write_text(
            f"#!/bin/sh\nexec {shlex.quote(sys.executable)} {shlex.quote(str(helper))} \"$@\"\n"
        )
        gh.chmod(0o755)
        self.environment = {
            "PATH": str(self.root) + os.pathsep + os.defpath,
            "LOOKUP_RESPONSE": str(self.payload),
            "LOOKUP_ARGS": str(self.arguments),
        }

    def lookup(self, payload):
        self.payload.write_text(json.dumps(payload))
        with patch.dict(os.environ, self.environment, clear=True):
            result = release.open_bump_pr()
        self.assertEqual(json.loads(self.arguments.read_text()), [
            "pr", "list", "--base", "main", "--head", release.BRANCH, "--state", "open",
            "--json", "number,isDraft,headRefOid,isCrossRepository",
        ])
        return result

    @staticmethod
    def pr(number=42, cross=False):
        return {
            "number": number, "isDraft": True, "headRefOid": "a" * 40,
            "isCrossRepository": cross,
        }

    def test_no_open_bump_is_an_empty_array_not_empty_json(self):
        self.assertIsNone(self.lookup([]))

    def test_same_repository_bump_is_preserved_including_opaque_extra_fields(self):
        own = {**self.pr(), "futureField": {"opaque": "kept"}}
        self.assertEqual(self.lookup([own]), own)

    def test_matching_fork_is_not_armed_and_does_not_hide_own_bump(self):
        own = self.pr(43)
        self.assertEqual(self.lookup([self.pr(cross=True), own]), own)

    def test_only_matching_forks_means_no_own_bump(self):
        self.assertIsNone(self.lookup([self.pr(cross=True)]))

    def test_unexpected_container_fails_closed(self):
        for payload in (None, {}, "unexpected"):
            with self.subTest(payload=payload), self.assertRaises(ValueError):
                self.lookup(payload)

    def test_missing_or_unsupported_repository_identity_fails_closed(self):
        for record in (None, {}, *(
            {**self.pr(), "isCrossRepository": value} for value in (None, "false", 0, 1)
        )):
            with self.subTest(record=record), self.assertRaises(ValueError):
                self.lookup([record])

    def test_failed_gh_does_not_masquerade_as_no_open_bump(self):
        self.environment["LOOKUP_FAIL"] = "1"
        with self.assertRaises(subprocess.CalledProcessError):
            self.lookup([])


if __name__ == "__main__":
    unittest.main()
