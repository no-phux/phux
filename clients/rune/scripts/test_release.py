import unittest

from release import bump, next_version, notes, shippable


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


if __name__ == "__main__":
    unittest.main()
