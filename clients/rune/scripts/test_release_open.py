"""Exercise the actual subprocess/JSON boundary, without contacting GitHub."""
import json
import os
from pathlib import Path
import shlex
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
    sys.stderr.write("gh: lookup refused\\n")
    sys.stdout.write(os.environ.get("LOOKUP_FAIL_STDOUT", ""))
    sys.exit(7)
if "LOOKUP_RAW" in os.environ:
    sys.stdout.write(os.environ["LOOKUP_RAW"])
    sys.exit(0)
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

    def invoke(self, payload):
        self.payload.write_text(json.dumps(payload))
        with patch.dict(os.environ, self.environment, clear=True):
            return release.open_bump_pr()

    def assert_invoked_lookup(self):
        self.assertEqual(json.loads(self.arguments.read_text()), [
            "pr", "list", "--base", "main", "--head", release.BRANCH, "--state", "open",
            "--json", "number,isDraft,headRefOid,isCrossRepository",
        ])

    def lookup(self, payload):
        result = self.invoke(payload)
        self.assert_invoked_lookup()
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
        self.environment["LOOKUP_FAIL_STDOUT"] = "partial gh body"
        with self.assertRaises(RuntimeError) as caught:
            self.invoke([])
        self.assert_invoked_lookup()
        message = str(caught.exception)
        self.assertIn("gh pr list", message)
        self.assertIn("exited 7", message)
        self.assertIn("gh: lookup refused", message)
        self.assertIn("partial gh body", message)
        self.assertNotIn("no open bump", message)

    def test_empty_stdout_with_exit_zero_is_not_no_open_bump(self):
        cases = {"empty": "", "whitespace": " \n\t"}
        for name, body in cases.items():
            with self.subTest(body=name):
                self.environment["LOOKUP_RAW"] = body
                with self.assertRaises(RuntimeError) as caught:
                    self.invoke([])
                self.assert_invoked_lookup()
                message = str(caught.exception)
                self.assertIn("gh pr list", message)
                self.assertIn("exited 0", message)
                self.assertIn("stderr:", message)
                self.assertNotIn("no open bump", message)
                if body:
                    self.assertIn("stdout was whitespace", message)
                    self.assertIn(repr(body), message)
                else:
                    self.assertIn("stdout was empty", message)

    def test_non_json_stdout_fails_with_the_raw_output(self):
        raw = "not json { oops"
        self.environment["LOOKUP_RAW"] = raw
        with self.assertRaises(ValueError) as caught:
            self.invoke([])
        self.assert_invoked_lookup()
        self.assertIn(raw, str(caught.exception))


if __name__ == "__main__":
    unittest.main()
