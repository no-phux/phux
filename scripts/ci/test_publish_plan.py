#!/usr/bin/env python3
"""Publish decisions stay deterministic: no network, no clock."""

import unittest

from publish_plan import decide, parse_tag, split_lanes, validation_state


def rel(tag, draft, sha):
    return {"tag": tag, "draft": draft, "sha": sha}


def green(_sha):
    return "success"


class ParseTagTests(unittest.TestCase):
    def test_root_cockpit_and_integration_tags(self):
        self.assertEqual(parse_tag("v0.42.0")["component"], "phux")
        self.assertEqual(parse_tag("cockpit-v0.28.0")["component"], "cockpit")
        self.assertEqual(parse_tag("pi-extension-v0.3.0")["component"], "pi-extension")

    def test_moving_channel_is_not_a_version(self):
        self.assertIsNone(parse_tag("next"))

    def test_only_desktop_alpha_tags_are_supported_prereleases(self):
        self.assertEqual(parse_tag("desktop-v0.1.0-alpha.1")["component"], "desktop")
        for tag in ("v0.1.0-alpha.1", "cockpit-v0.1.0-alpha.1",
                    "pi-extension-v0.1.0-alpha.1", "desktop-v0.1.0",
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
            rel("cockpit-v0.28.0", True, "abc"),
            rel("v0.41.0", False, "old"),
        ]
        plan = decide(releases, green, event_sha="abc")
        self.assertEqual(plan["publish"], ["v0.42.0", "cockpit-v0.28.0"])
        self.assertFalse(plan["wait"])
        self.assertIsNone(plan["fail"])

    def test_event_waits_while_ci_is_still_running(self):
        plan = decide([rel("v0.42.0", True, "abc")], lambda _sha: "pending", event_sha="abc")
        self.assertEqual(plan["publish"], [])
        self.assertTrue(plan["wait"])
        self.assertIsNone(plan["fail"])

    def test_event_does_not_publish_when_ci_failed(self):
        plan = decide([rel("cockpit-v0.28.0", True, "abc")], lambda _sha: "cancelled", event_sha="abc")
        self.assertEqual(plan["publish"], [])
        self.assertIn("cancelled", plan["fail"])

    def test_event_ignores_a_draft_on_some_other_commit(self):
        plan = decide([rel("v0.42.0", True, "other")], green, event_sha="abc")
        self.assertEqual(plan["publish"], [])
        self.assertFalse(plan["wait"])

    def test_superseded_drafts_are_deleted_not_shipped(self):
        releases = [
            rel("v0.42.0", False, "new"),
            rel("v0.39.0", True, "old"),
            rel("cockpit-v0.27.1", False, "pub"),
            rel("cockpit-v0.24.0", True, "oldc"),
            rel("next", True, "moving"),
        ]
        plan = decide(releases, green, event_sha="old")
        self.assertEqual(plan["publish"], [])
        self.assertCountEqual(plan["delete"], ["v0.39.0", "cockpit-v0.24.0"])

    def test_reconcile_ships_the_newest_green_draft_and_reports_the_blocked_one(self):
        releases = [
            rel("cockpit-v0.27.1", False, "pub"),
            rel("cockpit-v0.28.0", True, "green"),
            rel("pi-extension-v0.3.0", True, "red"),
        ]

        def ci_state(sha):
            return "success" if sha == "green" else "failure"

        plan = decide(releases, ci_state)
        self.assertEqual(plan["publish"], ["cockpit-v0.28.0"])
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
            [rel("cockpit-v0.28.0", True, "abc")],
            lambda _sha: "failure",
            dispatch_tag="cockpit-v0.28.0",
        )
        self.assertEqual(plan["publish"], [])
        self.assertIn("failure", plan["fail"])

    def test_dispatch_refuses_a_superseded_draft(self):
        releases = [rel("v0.42.0", False, "new"), rel("v0.39.0", True, "old")]
        plan = decide(releases, green, dispatch_tag="v0.39.0")
        self.assertIn("older", plan["fail"])

    def test_lanes_split_root_cockpit_and_integrations(self):
        lanes = split_lanes(["v0.42.0", "cockpit-v0.28.0", "pi-extension-v0.3.0"])
        self.assertEqual(lanes["phux"], "v0.42.0")
        self.assertEqual(lanes["cockpit"], "cockpit-v0.28.0")
        self.assertEqual(lanes["integration"], ["pi-extension-v0.3.0"])

    def test_alpha_counter_order_is_numeric_and_never_an_integration(self):
        releases = [
            rel("desktop-v0.1.0-alpha.1", False, "old"),
            rel("desktop-v0.1.0-alpha.9", True, "nine"),
            rel("desktop-v0.1.0-alpha.10", True, "ten"),
        ]
        plan = decide(releases, green)
        self.assertEqual(plan["publish"], ["desktop-v0.1.0-alpha.10"])
        lanes = split_lanes(plan["publish"])
        self.assertEqual(lanes["desktop"], "desktop-v0.1.0-alpha.10")
        self.assertEqual(lanes["integration"], [])
        self.assertEqual(lanes["phux"], "")

    def test_stable_desktop_is_ignored_without_superseding_alphas(self):
        stable = "desktop-v0.1.0"
        alpha = "desktop-v0.1.0-alpha.10"
        for draft in (False, True):
            with self.subTest(draft=draft):
                plan = decide([
                    rel(stable, draft, "stable"),
                    rel(alpha, True, "alpha"),
                ], green)
                self.assertEqual(plan["publish"], [alpha])
                self.assertEqual(plan["delete"], [])
        self.assertEqual(split_lanes([alpha, stable])["desktop"], alpha)
        plan = decide([rel(stable, True, "stable")], green, dispatch_tag=stable)
        self.assertEqual(plan["publish"], [])
        self.assertIsNotNone(plan["fail"])

    def test_alpha_dispatch_requires_its_exact_commit_ci(self):
        tag = "desktop-v0.1.0-alpha.1"
        rows = [rel(tag, True, "alpha")]
        self.assertEqual(decide(rows, green, event_sha="other")["publish"], [])
        self.assertEqual(decide(rows, green, dispatch_tag=tag)["publish"], [tag])
        self.assertIn("failure", decide(rows, lambda _: "failure", dispatch_tag=tag)["fail"])


if __name__ == "__main__":
    unittest.main()
