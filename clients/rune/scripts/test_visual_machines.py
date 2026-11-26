"""The actual Machines exporter matrix; no native UIKit qualification."""
import unittest

import visual_review as review


class MachineColourInventory(unittest.TestCase):
    def test_matrix_keeps_keyboard_large_text_and_focus(self):
        cases = review.machine_colour_inventory()
        self.assertEqual(len(cases), 72)
        for form in ("phone", "small"):
            for state in ("ordinary", "large", "keyboard", "large-keyboard"):
                for theme in review.THEMES:
                    for scale in (1, 2):
                        stem = f"settings/machine-colour-{form}-{state}-{theme}-{scale}x"
                        self.assertIn(f"{stem}.png", cases)
                        self.assertIn(f"{stem}-focused.png", cases)


if __name__ == "__main__":
    unittest.main()
