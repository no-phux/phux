"""The maintained sidebar hierarchy export matrix and its actual zoom geometry."""
from itertools import product
import unittest

import visual_review as review


class SidebarHierarchyInventory(unittest.TestCase):
    def test_native_inset_and_interactive_states(self):
        cases = review.required_inventory()
        self.assertEqual(sum(name.startswith("navigation/sidebar-hierarchy-") for name in cases), 72)
        for theme, mode, state, (width, height) in product(
            ("dark", "light"), ("normal", "large", "forced"),
            ("expanded", "collapsed", "children", "hosts", "focused", "empty"),
            ((1100, 720), (640, 360)),
        ):
            stem = f"sidebar-hierarchy-{theme}-{mode}-{state}-{width}x{height}-2x"
            logical_w, logical_h, scale = cases[f"navigation/{stem}.png"]
            self.assertEqual((round(logical_w * scale), round(logical_h * scale)),
                             (width * 2, height * 2))
            if mode == "large":
                self.assertAlmostEqual(logical_w, width / 1.4, delta=0.0001)
                self.assertAlmostEqual(logical_h, height / 1.4, delta=0.0001)
                self.assertAlmostEqual(scale, 2.8, delta=0.000001)
            else:
                self.assertEqual((logical_w, logical_h, scale), (width, height, 2))
            self.assertEqual(review.exporter_for("navigation", stem),
                             "sidebar_hierarchy_visual_review_frames")


if __name__ == "__main__":
    unittest.main()
