#!/usr/bin/env python3
"""Exercise Linear reporting's publication gate, tagged notes, and issue range.

All Git history is disposable; no credentials or Linear mutations are needed.
"""

import base64
import http.server
import os
from pathlib import Path
import re
import subprocess
import tempfile
import textwrap
import threading
import unittest
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT / ".github/workflows/linear-release.yml").read_text()


def shell_step(name):
    blocks = re.split(r"(?=^      - )", WORKFLOW, flags=re.M)[1:]
    block = next(block for block in blocks if block.startswith(f"      - name: {name}\n"))
    return textwrap.dedent(block.split("        run: |\n", 1)[1]).rstrip() + "\n"


class LinearReleaseWorkflowTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="phux-linear-report-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.git("init", "--quiet")
        self.git("config", "user.email", "release@example.invalid")
        self.git("config", "user.name", "Release Test")
        self.git("config", "core.hooksPath", "/dev/null")
        self.commit("initial", {"README": "initial\n"})

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.root, text=True).strip()

    def commit(self, subject, files):
        for name, content in files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        self.git("add", ".")
        self.git("commit", "--quiet", "-m", subject)
        return self.git("rev-parse", "HEAD")

    def run_step(self, name, **environment):
        output = self.root / "step-output"
        output.write_text("")
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("PHUX_", "GIT_")) and key not in ("GH_TOKEN", "LINEAR_ACCESS_KEY")}
        env.update(GITHUB_OUTPUT=str(output), **environment)
        result = subprocess.run(["bash", "-c", shell_step(name)], cwd=self.root,
                                env=env, text=True, capture_output=True)
        values = dict(line.split("=", 1) for line in output.read_text().splitlines())
        return result, values

    def test_recovery_range_ignores_newer_and_other_component_tags(self):
        self.commit("root previous", {"root": "old\n"})
        self.git("tag", "v0.47.0")
        self.commit("cockpit previous", {"clients/cockpit/change": "old\n"})
        self.git("tag", "cockpit-v0.20.0")
        self.commit("fix: PHA-406 root change", {"root": "new\n"})
        self.git("tag", "v0.48.0")
        self.commit("fix: PHA-407 cockpit change", {"clients/cockpit/change": "new\n"})
        self.git("tag", "cockpit-v0.21.0")
        self.commit("later root release", {"root": "later\n"})
        self.git("tag", "v0.49.0")
        for tag, component, expected in (("v0.48.0", "phux", "v0.47.0"),
                                          ("cockpit-v0.21.0", "cockpit", "cockpit-v0.20.0")):
            with self.subTest(tag=tag):
                self.git("checkout", "--quiet", tag)
                result, values = self.run_step("Resolve the tagged issue scan range", COMPONENT=component)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(values["base"], expected)
                subjects = self.git("log", "--format=%s", f"{values['base']}..HEAD")
                self.assertIn("PHA-406" if component == "phux" else "PHA-407", subjects)
                self.assertNotIn("later root release", subjects)

    def test_first_component_release_has_explicit_ancestor_boundary(self):
        initial = self.git("rev-parse", "HEAD")
        self.commit("first cockpit release", {"clients/cockpit/change": "first\n"})
        self.git("tag", "cockpit-v0.1.0")
        result, values = self.run_step("Resolve the tagged issue scan range", COMPONENT="cockpit")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(values["base"], initial)

    def test_tagged_notes_survive_old_tag_recovery(self):
        tagged = "## [0.48.0]\n\n### Bug Fixes\n\n* shipped PHA-406\n"
        self.commit("released", {"CHANGELOG.md": tagged + "\n## [0.47.0]\n\n* older\n"})
        self.git("tag", "v0.48.0")
        self.commit("later notes", {"CHANGELOG.md": "## [0.49.0]\n\n* not shipped in 0.48.0\n"})
        runner = self.root / "runner"
        runner.mkdir()
        (runner / "extract-changelog-section.py").write_bytes(
            (ROOT / "scripts/ci/extract_changelog_section.py").read_bytes())
        self.git("checkout", "--quiet", "v0.48.0")
        result, _ = self.run_step("Write Linear release notes from the tagged changelog",
                                  TAG="v0.48.0", CHANGELOG="CHANGELOG.md", RUNNER_TEMP=str(runner))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "release-notes.md").read_text(), tagged)

    def test_unpublished_and_unknown_releases_are_rejected(self):
        # Only the GitHub API boundary is replaced; execute the actual shell gate.
        binary = self.root / "bin"
        binary.mkdir()
        gh = binary / "gh"
        gh.write_text('#!/bin/sh\n[ "$API_RESULT" != missing ] || exit 7\nprintf "%s\\n" "$API_RESULT"\n')
        gh.chmod(0o755)
        for response, expected in (("false", 0), ("true", 1), ("missing", 7), ("null", 1)):
            with self.subTest(response=response):
                result, _ = self.run_step("Require a published GitHub release", TAG="v0.48.0",
                                          GITHUB_REPOSITORY="example/disposable", API_RESULT=response,
                                          PATH=f"{binary}:{os.environ['PATH']}")
                self.assertEqual(result.returncode, expected, result.stderr)

    def test_tag_fetch_resets_checkout_authorization_header(self):
        self.git("tag", "v0.48.0")
        expected_commit = self.git("rev-parse", "HEAD")
        self.commit("later source", {"README": "later\n"})
        bare = self.root / "fixture.git"
        subprocess.run(["git", "clone", "--bare", "--quiet", str(self.root), str(bare)], check=True)
        self.git("tag", "--delete", "v0.48.0")
        helper = self.root / "scripts/ci/extract_changelog_section.py"
        helper.parent.mkdir(parents=True)
        helper.write_bytes((ROOT / "scripts/ci/extract_changelog_section.py").read_bytes())
        runner = self.root / "runner"
        runner.mkdir()
        expected_header = "basic " + base64.b64encode(b"x-access-token:fixture-token").decode()
        project_root = str(self.root)

        class GitHTTP(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def handle_git(self):
                authorization = self.headers.get_all("Authorization", [])
                if authorization != [expected_header]:
                    self.send_error(400, "duplicate or incorrect Authorization")
                    return
                url = urlsplit(self.path)
                length = int(self.headers.get("Content-Length", 0))
                env = dict(os.environ, GIT_PROJECT_ROOT=project_root, GIT_HTTP_EXPORT_ALL="1",
                           PATH_INFO=url.path, QUERY_STRING=url.query, REQUEST_METHOD=self.command,
                           CONTENT_TYPE=self.headers.get("Content-Type", ""),
                           CONTENT_LENGTH=str(length), HTTP_GIT_PROTOCOL=self.headers.get("Git-Protocol", ""))
                response = subprocess.check_output(["git", "http-backend"], env=env,
                                                   input=self.rfile.read(length))
                fields, body = response.split(b"\r\n\r\n", 1)
                self.send_response(200)
                for line in fields.split(b"\r\n"):
                    key, value = line.decode().split(": ", 1)
                    self.send_header(key, value)
                self.end_headers()
                self.wfile.write(body)

            do_GET = handle_git
            do_POST = handle_git

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), GitHTTP)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            url = f"http://127.0.0.1:{server.server_port}/"
            # Model checkout's existing credential and substitute only the
            # network endpoint; execute real Git and the complete workflow step.
            self.git("config", f"http.{url}.extraheader", "AUTHORIZATION: basic prior-checkout-token")
            script = shell_step("Fetch the release tag from GitHub, bypassing the runner git mirror")
            script = script.replace("https://github.com/", url)
            env = {key: value for key, value in os.environ.items()
                   if not key.startswith(("PHUX_", "GIT_"))}
            env.update(GH_TOKEN="fixture-token", GITHUB_REPOSITORY="fixture.git", TAG="v0.48.0",
                       RUNNER_TEMP=str(runner), GIT_TERMINAL_PROMPT="0")
            result = subprocess.run(["bash", "-c", script], cwd=self.root, env=env,
                                    text=True, capture_output=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(self.git("rev-parse", "HEAD"), expected_commit)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == "__main__":
    unittest.main()
