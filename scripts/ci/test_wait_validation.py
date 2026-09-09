#!/usr/bin/env python3
"""Release fan-out cannot pass on another SHA, workflow, or failed run."""

import unittest

from wait_validation import validation_state, wait


class ValidationTests(unittest.TestCase):
    def setUp(self):
        self.run = {"id": 1, "head_sha": "abc", "head_branch": "main", "event": "push",
                    "path": ".github/workflows/ci.yml", "status": "completed", "conclusion": "success"}

    def state(self, runs):
        return validation_state(runs, "abc", ".github/workflows/ci.yml")

    def test_exact_success(self):
        self.assertEqual(self.state([self.run]), "success")

    def test_unrelated_run_cannot_release(self):
        for field, value in [("head_sha", "def"), ("event", "pull_request"),
                             ("head_branch", "topic"), ("path", "other.yml")]:
            with self.subTest(field=field):
                self.assertEqual(self.state([{**self.run, field: value}]), "pending")

    def test_newer_failed_or_running_attempt_wins(self):
        self.assertEqual(self.state([self.run, {**self.run, "id": 2, "conclusion": "failure"}]), "failure")
        self.assertEqual(self.state([{**self.run, "status": "in_progress"}]), "pending")

    def test_missing_runs_timeout(self):
        ticks = iter([0, 0, 16])
        with self.assertRaises(TimeoutError):
            wait("owner/project", "abc", "ci.yml", 15,
                 fetch=lambda _: {"workflow_runs": []}, clock=lambda: next(ticks), sleep=lambda _: None)

    def test_failed_run_stops_immediately(self):
        with self.assertRaisesRegex(RuntimeError, "failure"):
            wait("owner/project", "abc", ".github/workflows/ci.yml", 15,
                 fetch=lambda _: {"workflow_runs": [{**self.run, "conclusion": "failure"}]})


if __name__ == "__main__":
    unittest.main()
