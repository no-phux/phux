#!/usr/bin/env python3
"""Publish decisions stay deterministic: no network, no clock."""

import unittest

from publish_plan import decide, lane, parse_tag, split_lanes, validation_state


def rel(tag, draft, sha):
    return {"tag": tag, "draft": draft, "sha": sha}


def green(_sha):
    return "success"


class ParseTagTests(unittest.TestCase):
    def test_root_desktop_and_integration_tags(self):
        self.assertEqual(parse_tag("v0.42.0")["component"], "phux")
        self.assertEqual(parse_tag("desktop-v0.28.0")["component"], "desktop")
        self.assertEqual(parse_tag("pi-extension-v0.3.0")["component"], "pi-extension")

    def test_moving_channel_is_not_a_version(self):
        self.assertIsNone(parse_tag("next"))

    def test_stable_desktop_is_a_version_and_prereleases_are_not(self):
        self.assertEqual(parse_tag("desktop-v0.1.0")["component"], "desktop")
        for tag in ("v0.1.0-alpha.1", "cockpit-v0.1.0-alpha.1",
                    "pi-extension-v0.1.0-alpha.1", "desktop-v0.1.0-alpha.1",
                    "desktop-v0.1.0-alpha.0", "desktop-v0.1.0-alpha.01",
                    "desktop-v0.1.0-beta.1", "desktop-v0.1.0-alpha.1+build"):
            with self.subTest(tag=tag):
                self.assertIsNone(parse_tag(tag))


class ValidationStateTests(unittest.TestCase):
    def setUp(self):
        self.run = {
            "id": 1, "head_sha": "abc", "head_branch": "main", "event": "push",
            "path": ".github/workflows/ci.yml", "status": "completed", "conclusion": "success",
        }

    def state(self, runs):
        return validation_state(runs, "abc", ".github/workflows/ci.yml")

    def test_exact_success(self):
        self.assertEqual(self.state([self.run]), "success")

    def test_unrelated_run_cannot_release(self):
        for field, value in [
            ("head_sha", "def"), ("event", "pull_request"),
            ("head_branch", "topic"), ("path", "other.yml"),
        ]:
            with self.subTest(field=field):
                self.assertEqual(self.state([{**self.run, field: value}]), "pending")

    def test_newer_failed_or_running_attempt_wins(self):
        failed = {**self.run, "id": 2, "conclusion": "failure"}
        self.assertEqual(self.state([self.run, failed]), "failure")
        self.assertEqual(self.state([{**self.run, "status": "in_progress"}]), "pending")


class DecideTests(unittest.TestCase):
    def test_event_publishes_every_component_on_a_green_commit(self):
        releases = [
            rel("v0.42.0", True, "abc"),
            rel("desktop-v0.28.0", True, "abc"),
            rel("v0.41.0", False, "old"),
        ]
        plan = decide(releases, green, event_sha="abc")
        self.assertEqual(plan["publish"], ["v0.42.0", "desktop-v0.28.0"])
        self.assertFalse(plan["wait"])
        self.assertIsNone(plan["fail"])

    def test_event_waits_while_ci_is_still_running(self):
        plan = decide([rel("v0.42.0", True, "abc")], lambda _sha: "pending", event_sha="abc")
        self.assertEqual(plan["publish"], [])
        self.assertTrue(plan["wait"])
        self.assertIsNone(plan["fail"])

    def test_event_does_not_publish_when_ci_failed(self):
        plan = decide([rel("desktop-v0.28.0", True, "abc")], lambda _sha: "cancelled", event_sha="abc")
        self.assertEqual(plan["publish"], [])
        self.assertIn("cancelled", plan["fail"])

    def test_event_sweeps_a_draft_left_by_a_cancelled_run(self):
        # GitHub keeps one pending run per concurrency group and cancels the
        # older one, so the publish run a draft waits on can vanish before it
        # starts. The next event must ship that draft instead of stranding it
        # until the daily reconcile.
        plan = decide([rel("v0.42.0", True, "other")], green, event_sha="abc")
        self.assertEqual(plan["publish"], ["v0.42.0"])
        self.assertFalse(plan["wait"])
        self.assertIsNone(plan["fail"])

    def test_event_does_not_ship_a_straggler_whose_ci_is_not_green(self):
        def ci_state(sha):
            return "success" if sha == "abc" else "failure"

        plan = decide([rel("v0.42.0", True, "other")], ci_state, event_sha="abc")
        self.assertEqual(plan["publish"], [])
        self.assertIn("v0.42.0", plan["blocked"])
        self.assertIsNone(plan["fail"])

    def test_superseded_drafts_are_deleted_not_shipped(self):
        releases = [
            rel("v0.42.0", False, "new"),
            rel("v0.39.0", True, "old"),
            rel("desktop-v0.27.1", False, "pub"),
            rel("desktop-v0.24.0", True, "oldd"),
            rel("next", True, "moving"),
        ]
        plan = decide(releases, green, event_sha="old")
        self.assertEqual(plan["publish"], [])
        self.assertCountEqual(plan["delete"], ["v0.39.0", "desktop-v0.24.0"])

    def test_reconcile_ships_the_newest_green_draft_and_reports_the_blocked_one(self):
        releases = [
            rel("desktop-v0.27.1", False, "pub"),
            rel("desktop-v0.28.0", True, "green"),
            rel("pi-extension-v0.3.0", True, "red"),
        ]

        def ci_state(sha):
            return "success" if sha == "green" else "failure"

        plan = decide(releases, ci_state)
        self.assertEqual(plan["publish"], ["desktop-v0.28.0"])
        self.assertIn("pi-extension-v0.3.0", plan["blocked"])
        self.assertIsNone(plan["fail"])

    def test_reconcile_skips_a_draft_whose_ci_has_not_finished(self):
        plan = decide([rel("v0.42.0", True, "abc")], lambda _sha: "pending")
        self.assertEqual(plan["publish"], [])
        self.assertEqual(plan["blocked"], "")

    def test_only_the_newest_unpublished_draft_is_a_candidate(self):
        releases = [
            rel("v0.41.0", False, "pub"),
            rel("v0.42.0", True, "mid"),
            rel("v0.43.0", True, "tip"),
        ]
        plan = decide(releases, green)
        self.assertEqual(plan["publish"], ["v0.43.0"])

    def test_dispatch_of_a_published_tag_is_a_no_op(self):
        plan = decide([rel("v0.42.0", False, "abc")], green, dispatch_tag="v0.42.0")
        self.assertEqual(plan["publish"], [])
        self.assertIsNone(plan["fail"])

    def test_dispatch_refuses_a_tag_ci_did_not_pass(self):
        plan = decide(
            [rel("desktop-v0.28.0", True, "abc")],
            lambda _sha: "failure",
            dispatch_tag="desktop-v0.28.0",
        )
        self.assertEqual(plan["publish"], [])
        self.assertIn("failure", plan["fail"])

    def test_dispatch_refuses_a_superseded_draft(self):
        releases = [rel("v0.42.0", False, "new"), rel("v0.39.0", True, "old")]
        plan = decide(releases, green, dispatch_tag="v0.39.0")
        self.assertIn("older", plan["fail"])

    def test_lanes_split_root_desktop_and_integrations(self):
        lanes = split_lanes(["v0.42.0", "desktop-v0.28.0", "pi-extension-v0.3.0"])
        self.assertEqual(lanes["phux"], "v0.42.0")
        self.assertEqual(lanes["desktop"], "desktop-v0.28.0")
        self.assertEqual(lanes["integration"], ["pi-extension-v0.3.0"])

    def test_desktop_lane_takes_the_newest_stable_draft(self):
        releases = [
            rel("desktop-v0.1.0", False, "old"),
            rel("desktop-v0.2.0", True, "minor"),
            rel("desktop-v0.1.1", True, "patch"),
        ]
        plan = decide(releases, green)
        self.assertEqual(plan["publish"], ["desktop-v0.2.0"])
        lanes = split_lanes(plan["publish"])
        self.assertEqual(lanes["desktop"], "desktop-v0.2.0")
        self.assertEqual(lanes["integration"], [])
        self.assertEqual(lanes["phux"], "")

    def test_archived_cockpit_never_publishes_or_falls_through_to_integrations(self):
        retired = [rel("cockpit-v0.28.0", True, "retired"),
                   rel("cockpit-v0.27.0", False, "old"),
                   rel("cockpit-v0.24.0", True, "older")]
        for options in ({}, {"event_sha": "retired"}, {"dispatch_tag": "cockpit-v0.28.0"}):
            with self.subTest(options=options):
                plan = decide(retired, green, **options)
                self.assertEqual(plan["publish"], [])
                self.assertEqual(plan["delete"], [])
        self.assertIn("no release", decide(retired, green, dispatch_tag="cockpit-v0.28.0")["fail"])
        self.assertIsNone(parse_tag("cockpit-v0.28.0"))
        self.assertEqual(split_lanes(["cockpit-v0.28.0"]),
                         {"phux": "", "desktop": "", "integration": []})
        with self.assertRaises(ValueError):
            lane("cockpit")

    def test_reconcile_ignores_retired_releases_but_recovers_surviving_lanes(self):
        rows = [rel("cockpit-v0.28.0", True, "retired"),
                rel("v0.42.0", True, "green"), rel("desktop-v0.2.0", True, "green"),
                rel("pi-extension-v0.3.0", True, "green")]
        seen = []

        def ci_state(sha):
            seen.append(sha)
            return "success"

        plan = decide(rows, ci_state)
        self.assertNotIn("retired", seen)
        self.assertCountEqual(plan["publish"], ["v0.42.0", "desktop-v0.2.0", "pi-extension-v0.3.0"])

    def test_stable_desktop_supersedes_an_older_stable_draft(self):
        stable = "desktop-v0.2.0"
        older = "desktop-v0.1.0"
        plan = decide([rel(stable, False, "stable"), rel(older, True, "old")], green)
        self.assertEqual(plan["publish"], [])
        self.assertEqual(plan["delete"], [older])

    def test_desktop_dispatch_requires_its_exact_commit_ci(self):
        tag = "desktop-v0.1.0"
        rows = [rel(tag, True, "desktop")]
        self.assertEqual(decide(rows, green, dispatch_tag=tag)["publish"], [tag])
        self.assertIn("failure", decide(rows, lambda _: "failure", dispatch_tag=tag)["fail"])
        # An event for any other commit sweeps it like every other green draft.
        self.assertEqual(decide(rows, green, event_sha="other")["publish"], [tag])


if __name__ == "__main__":
    unittest.main()
