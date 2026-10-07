#!/usr/bin/env python3
"""Artifact workflows build the commit their run names, never a moving branch.

phux-mobile resolves a workflow run by its headSha and then fails closed when
the artifact's provenance names another commit (phux-dm8ik). A checkout of
`github.ref` builds whatever the branch is when the job STARTS, so a queued
push run stamped a later main's provenance under an earlier headSha.
"""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github/workflows"
MOBILE_ARTIFACT_WORKFLOWS = ("ffi-xcframework.yml", "ffi-android.yml")
BUILD_REF = "${{ inputs.tag || inputs.commit || github.sha }}"
MOVING_REF = re.compile(r"github\.(?:ref|head_ref|ref_name)\b")


class ArtifactProvenancePolicyTests(unittest.TestCase):
    def test_no_checkout_follows_the_triggering_branch(self):
        """A checkout `ref:` that names the event's branch resolves at job start."""
        for path in WORKFLOWS.glob("*.yml"):
            for value in re.findall(r"(?m)^\s+ref: (.+)$", path.read_text()):
                with self.subTest(workflow=path.name, ref=value):
                    self.assertIsNone(MOVING_REF.search(value))

    def test_mobile_artifacts_build_and_verify_the_run_commit(self):
        for name in MOBILE_ARTIFACT_WORKFLOWS:
            with self.subTest(workflow=name):
                body = (WORKFLOWS / name).read_text()
                self.assertIn(f"          ref: {BUILD_REF}\n", body)
                self.assertIn('test "$(git rev-parse HEAD)" = "$expected"', body)
                self.assertIn("name: Verify provenance names the build commit", body)
                self.assertRegex(body, r'grep -qx "phux[-_]rev[ =]\$BUILD_SHA"')

    def test_mobile_artifacts_dispatch_takes_an_explicit_commit(self):
        for name in MOBILE_ARTIFACT_WORKFLOWS:
            with self.subTest(workflow=name):
                body = (WORKFLOWS / name).read_text()
                dispatch = body.split("  workflow_dispatch:\n", 1)[1].split("\npermissions:", 1)[0]
                self.assertIn("      commit:\n", dispatch)
                # A commit other than the run's headSha must not reuse the
                # canonical artifact names a headSha lookup would download.
                self.assertIn('suffix="-$expected"', body)
                for artifact in re.findall(r"(?m)^\s+name: (Phux\S*FFI-\S+)$", body):
                    self.assertTrue(artifact.endswith("${{ env.ARTIFACT_SUFFIX }}"), artifact)

    def test_mobile_artifact_runs_are_grouped_per_commit_and_never_cancelled(self):
        """A ref-keyed group replaces a pending run, so its commit never gets artifacts."""
        for name in MOBILE_ARTIFACT_WORKFLOWS:
            with self.subTest(workflow=name):
                body = (WORKFLOWS / name).read_text()
                concurrency = body.split("\nconcurrency:\n", 1)[1].split("\n\n", 1)[0]
                self.assertIn(BUILD_REF, concurrency)
                self.assertIsNone(MOVING_REF.search(concurrency))
                self.assertIn("cancel-in-progress: false", concurrency)


if __name__ == "__main__":
    unittest.main()
