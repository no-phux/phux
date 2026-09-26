#!/usr/bin/env python3
"""The dispatcher waits for the run it just created, not an older one."""

import unittest
from unittest import mock

from dispatch_integration_publishes import dispatch


class DispatchTests(unittest.TestCase):
    def test_returns_the_new_workflow_dispatch_run(self):
        seen = {"calls": 0}

        def list_runs(_repo):
            seen["calls"] += 1
            if seen["calls"] == 1:
                return [{"databaseId": 1, "event": "workflow_dispatch"}]
            return [
                {"databaseId": 1, "event": "workflow_dispatch"},
                {"databaseId": 9, "event": "workflow_dispatch"},
            ]

        with mock.patch("dispatch_integration_publishes.subprocess.check_call") as gh:
            run_id = dispatch(
                "no-phux/phux", "pi-extension-v0.3.0",
                list_runs=list_runs, sleep=lambda _seconds: None, clock=lambda: 0,
            )
        self.assertEqual(run_id, 9)
        gh.assert_called_once()
        args = gh.call_args.args[0]
        self.assertIn("agent-integration-release.yml", args)
        self.assertIn("tag=pi-extension-v0.3.0", args)
        self.assertIn("dry_run=false", args)


if __name__ == "__main__":
    unittest.main()
