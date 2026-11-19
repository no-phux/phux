import unittest

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

    def test_inside_the_six_hour_gate_still_waits(self):
        self.assertEqual(stuck_exit(STUCK_SECONDS, SHA, []), 0)
        self.assertEqual(stuck_exit(STUCK_SECONDS - 1, SHA, []), 0)
        self.assertEqual(stuck_exit(STUCK_SECONDS + 1, SHA, []), 1)


if __name__ == "__main__":
    unittest.main()
