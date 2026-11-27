"""Motion sidecars qualify frame clocks, not perceived smoothness."""
import json
import unittest

import test_visual_review as fixtures
import visual_review as review
from visual_motion import DISMISS_FRAMES, batched_swipe_inventory, dismiss_inventory, motion_frames


class MotionReview(unittest.TestCase):
    def setUp(self):
        self.fixture = fixtures.VisualReview()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)

    def test_motion_timing_is_required_and_bound_to_its_case_frame_and_clock(self):
        relative, geometry = next(iter(review.pane_motion_inventory().items()))
        self.fixture.populate({relative: geometry})
        path = (self.fixture.root / relative).with_suffix(".motion.json")
        original = path.read_text()
        path.unlink()
        with self.assertRaisesRegex(ValueError, "missing motion timing"):
            self.fixture.produce()
        path.write_text("[]")
        with self.assertRaisesRegex(ValueError, "motion must be an object"):
            self.fixture.produce()
        for field, value, message in (
            ("schema", True, "wrong motion case"),
            ("scenario", "another case", "wrong motion case"),
            ("stage", "unknown", "unknown motion stage"),
            ("stage", [], "unknown motion stage"),
            ("frame", 999, "wrong motion frame"),
            ("elapsed_ms", 999, "wrong motion clock"),
        ):
            with self.subTest(field=field):
                data = json.loads(original)
                data[field] = value
                path.write_text(json.dumps(data))
                with self.assertRaisesRegex(ValueError, message):
                    self.fixture.produce()
        path.write_text(original)
        case = next(case for case in self.fixture.produce()["cases"] if case["png"] == relative)
        self.assertEqual(len(case["motionSha256"]), 64)
        self.assertEqual(case["motion"], path.relative_to(self.fixture.root).as_posix())

    def test_batched_swipes_bind_the_final_move_and_release_timeline(self):
        inventory = batched_swipe_inventory()
        self.assertEqual(len(inventory), 80)
        self.fixture.populate(inventory)
        cases = self.fixture.produce()["cases"]
        for case in cases:
            if not case["png"].startswith("navigation/batched-swipe-"):
                continue
            self.assertEqual(case["exporter"], "batched_swipe_visual_review_frames")
            self.assertIn("motionSha256", case)
        relative = next(iter(inventory))
        path = (self.fixture.root / relative).with_suffix(".motion.json")
        data = json.loads(path.read_text())
        data["frame"] = 999
        path.write_text(json.dumps(data))
        with self.assertRaisesRegex(ValueError, "wrong motion frame"):
            review.case_record(self.fixture.root, self.fixture.root / relative)

    def test_dismiss_swipes_bind_both_let_goes_and_name_their_own_stage(self):
        inventory = dismiss_inventory()
        self.assertEqual(len(inventory), 432)
        self.fixture.populate(inventory)
        for case in self.fixture.produce()["cases"]:
            if not case["png"].startswith("navigation/dismiss-"):
                continue
            self.assertEqual(case["exporter"], "dismiss_motion_visual_review_frames")
            self.assertIn("motionSha256", case)
        # A stage that ends like another still resolves to its own frame.
        stem = "dismiss-sessions-left-dark-393x852-2x-reduced-false-short-release"
        frames = motion_frames(stem)
        stage = next(stage for stage in frames if stem.endswith(f"-{stage}"))
        self.assertEqual(frames[stage], DISMISS_FRAMES["short-release"])
