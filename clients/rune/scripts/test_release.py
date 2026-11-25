import json
import os
import subprocess
import unittest
from unittest.mock import call, patch

import release

from release import STUCK_SECONDS, already_reported, bump, next_version, notes, shippable, stuck_exit, stuck_line

SHA = "3bbef2bd820c1c16118c96d0d2e6fa2c216149c6"
OTHER = "e2f9f3d69e3dd5a45105761c55b23de160dfbef8"


class ReleaseTest(unittest.TestCase):
    def test_next_version_counts_alphas_then_patches(self):
        self.assertEqual(next_version("0.1.0-alpha.19"), "0.1.0-alpha.20")
        self.assertEqual(next_version("0.1.0"), "0.1.1")
        with self.assertRaises(ValueError):
            next_version("0.1.0-beta.1")

    def test_only_user_visible_commits_ship(self):
        subjects = [
            "fix(rune): show an acp question raised before rune connected (#2070)",
            "chore(rune): release 0.1.0-alpha.19",
            "feat!: breaking thing",
            "test(rune): drop document (#2062)",
        ]
        self.assertEqual(
            shippable(subjects),
            ["Show an acp question raised before rune connected (#2070)", "Breaking thing"],
        )

    def test_notes_open_with_the_header_preflight_wants(self):
        text = notes("0.1.0-alpha.20", "alpha.19", ["One"])
        self.assertTrue(text.startswith("**Phux Rune 0.1.0-alpha.20**: 1 change since alpha.19."))

    def test_bump_rewrites_the_package_and_its_lock_entry_only(self):
        manifest = '[package]\nname = "phux-rune"\nversion = "0.1.0-alpha.19"\n[deps]\nversion = "1"\n'
        lock = 'name = "other"\nversion = "1.0"\n\nname = "phux-rune"\nversion = "0.1.0-alpha.19"\n'
        m, l = bump(manifest, lock, "0.1.0-alpha.20")
        self.assertIn('version = "0.1.0-alpha.20"', m)
        self.assertIn('version = "1"', m)
        self.assertIn('name = "other"\nversion = "1.0"', l)
        self.assertIn('name = "phux-rune"\nversion = "0.1.0-alpha.20"', l)

    def test_stuck_line_is_what_later_runs_recognize(self):
        line = stuck_line("rune-v0.1.43", SHA, 7 * 3600)
        self.assertEqual(
            line,
            f"rune-v0.1.43: ci is not green on {SHA} after 7h; fix main, then rerun",
        )
        self.assertTrue(already_reported([line], SHA))

    def test_second_failure_on_the_same_sha_does_not_re_fail(self):
        # The run head can move (main's tip) while the release SHA in the
        # line stays put. The line is the marker, not the head SHA.
        log = (
            "step\tOne step of the Rune train\t"
            f"rune-v0.1.43: ci is not green on {SHA} after 7h; fix main, then rerun\n"
            f"head {OTHER}\n"
        )
        self.assertEqual(stuck_exit(STUCK_SECONDS + 1, SHA, [log]), 0)
        self.assertEqual(stuck_exit(8 * 3600, SHA, [log]), 0)

    def test_a_new_sha_still_fails(self):
        prior = f"rune-v0.1.42: ci is not green on {OTHER} after 7h; fix main, then rerun\n"
        self.assertFalse(already_reported([prior], SHA))
        self.assertEqual(stuck_exit(STUCK_SECONDS + 1, SHA, [prior]), 1)
        self.assertEqual(stuck_exit(STUCK_SECONDS + 1, SHA, []), 1)

    def test_an_unrelated_failure_on_the_same_sha_still_alerts(self):
        log = f"CalledProcessError while pushing {SHA}\n"
        self.assertEqual(stuck_exit(STUCK_SECONDS + 1, SHA, [log]), 1)

    def test_unreadable_history_still_fails(self):
        self.assertEqual(stuck_exit(STUCK_SECONDS + 1, SHA, None), 1)
        self.assertEqual(stuck_exit(STUCK_SECONDS, SHA, None), 0)


class BumpPrTest(unittest.TestCase):
    def setUp(self):
        self.pr = {"number": 42, "headRefOid": SHA, "isDraft": False}
        self.current = {"headRefOid": SHA, "isDraft": False, "state": "OPEN"}
        self.guard = call("bun", "tools/main.ts", "pr-ready-ci", "42", SHA, check=False)
        self.view = call("gh", "pr", "view", "42", "--json", "state,isDraft,headRefOid")
        self.merge = call("gh", "pr", "merge", "42", "--auto", "--squash", "--match-head-commit", SHA)
        self.prints = patch("builtins.print").start()
        self.addCleanup(patch.stopall)

    def test_draft_is_armed_once_without_guard_or_auto_merge(self):
        with patch("release.sh") as sh:
            self.assertEqual(release.reconcile_bump_pr({**self.pr, "isDraft": True}), 0)
        self.assertEqual(sh.call_args_list, [call("gh", "pr", "ready", "42")])

    def test_pending_failed_or_unreadable_guard_never_enables_auto_merge(self):
        for code in (1, 2, 3):
            with self.subTest(code=code), patch("release.sh") as sh:
                sh.return_value = subprocess.CompletedProcess([], code, "pending or failed", "detail")
                self.assertEqual(release.reconcile_bump_pr(self.pr), 0)
                self.assertEqual(sh.call_args_list, [self.guard])
                self.prints.assert_any_call("pending or faileddetail")

    def test_success_requeries_live_pr_and_binds_auto_merge_to_the_guarded_head(self):
        with patch("release.sh") as sh:
            sh.side_effect = [
                subprocess.CompletedProcess([], 0, "", ""),
                json.dumps(self.current),
                "",
            ]
            self.assertEqual(release.reconcile_bump_pr(self.pr), 0)
        self.assertEqual(sh.call_args_list, [self.guard, self.view, self.merge])

    def test_later_polls_gate_an_armed_pr_without_rearming_it(self):
        with patch("release.sh") as sh:
            sh.side_effect = [
                "",
                subprocess.CompletedProcess([], 1, "pending", ""),
                subprocess.CompletedProcess([], 0, "", ""),
                json.dumps(self.current),
                "",
            ]
            self.assertEqual(release.reconcile_bump_pr({**self.pr, "isDraft": True}), 0)
            self.assertEqual(release.reconcile_bump_pr(self.pr), 0)
            self.assertEqual(release.reconcile_bump_pr(self.pr), 0)
        self.assertEqual(sh.call_args_list, [
            call("gh", "pr", "ready", "42"), self.guard, self.guard, self.view, self.merge,
        ])

    def test_stale_head_current_draft_or_closed_pr_blocks_even_after_guard_success(self):
        for changed in ({"headRefOid": OTHER}, {"isDraft": True}, {"state": "CLOSED"}, {"state": "MERGED"}):
            with self.subTest(changed=changed), patch("release.sh") as sh:
                sh.side_effect = [
                    subprocess.CompletedProcess([], 0, "", ""),
                    json.dumps({**self.current, **changed}),
                ]
                self.assertEqual(release.reconcile_bump_pr(self.pr), 0)
                self.assertEqual(sh.call_args_list, [self.guard, self.view])

    def test_requery_error_fails_closed(self):
        with patch("release.sh") as sh:
            sh.side_effect = [
                subprocess.CompletedProcess([], 0, "", ""),
                subprocess.CalledProcessError(1, ["gh", "pr", "view"]),
            ]
            with self.assertRaises(subprocess.CalledProcessError):
                release.reconcile_bump_pr(self.pr)
        self.assertEqual(sh.call_args_list, [self.guard, self.view])

    def test_existing_bump_is_reconciled_before_tag_only_or_new_change_checks(self):
        for tag_only in ("", "1"):
            with self.subTest(tag_only=tag_only), patch.dict(os.environ, RUNE_RELEASE_TAG_ONLY=tag_only):
                with patch("release.open_bump_pr", return_value=self.pr):
                    with patch("release.reconcile_bump_pr", return_value=0) as reconcile:
                        with patch("release.sh") as sh:
                            self.assertEqual(release.bump_step("0.1.1"), 0)
                reconcile.assert_called_once_with(self.pr)
                sh.assert_not_called()

    def test_main_completion_without_existing_bump_never_opens_one(self):
        with patch.dict(os.environ, RUNE_RELEASE_TAG_ONLY="1"):
            with patch("release.open_bump_pr", return_value=None), patch("release.sh") as sh:
                self.assertEqual(release.bump_step("0.1.1"), 0)
        sh.assert_not_called()

    def test_new_bump_is_created_as_draft_then_armed_without_auto_merge(self):
        with patch.dict(os.environ, RUNE_RELEASE_TAG_ONLY=""):
            with patch("release.open_bump_pr", return_value=None), patch("release.Path") as path:
                path.return_value.read_text.return_value = "release notes"
                with patch("release.bump", return_value=("manifest", "lock")), patch("release.sh") as sh:
                    sh.return_value = "fix(rune): paint"
                    self.assertEqual(release.bump_step("0.1.1"), 0)
        commands = [args.args for args in sh.call_args_list]
        create = next(args for args in commands if args[:3] == ("gh", "pr", "create"))
        self.assertIn("--draft", create)
        self.assertEqual(commands[-1], ("gh", "pr", "ready", release.BRANCH))
        self.assertFalse(any(args[:3] == ("gh", "pr", "merge") for args in commands))
        self.assertFalse(any(args[0] == "bun" for args in commands))

    def test_main_completion_reaches_existing_pr_reconciliation(self):
        with patch.dict(os.environ, RUNE_RELEASE_TAG_ONLY="1"):
            with patch("release.Path"), patch("release.cargo_version", return_value="0.1.1"):
                with patch("release.sh", return_value="rune-v0.1.1"):
                    with patch("release.bump_step", return_value=0) as bump_step:
                        self.assertEqual(release.main(), 0)
        bump_step.assert_called_once_with("0.1.1")

    def test_lookup_is_scoped_to_same_repository_open_main_bumps(self):
        own = {**self.pr, "isCrossRepository": False}
        listed = subprocess.CompletedProcess([], 0, json.dumps([own]), "")
        with patch("release.sh", return_value=listed) as sh:
            self.assertEqual(release.open_bump_pr(), own)
        sh.assert_called_once_with(
            "gh", "pr", "list", "--base", "main", "--head", release.BRANCH, "--state", "open",
            "--json", "number,isDraft,headRefOid,isCrossRepository",
            check=False,
        )


if __name__ == "__main__":
    unittest.main()
