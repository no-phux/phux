"""Motion sidecars qualify frame clocks, not perceived smoothness."""
import json
import unittest

import test_visual_review as fixtures
import visual_review as review


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
