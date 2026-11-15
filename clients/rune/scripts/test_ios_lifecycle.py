"""Real OS lifecycle actions stay bounded and require fresh app receipts."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, call, patch

path = Path(__file__).resolve().parents[1] / "ios/qualify.py"
spec = importlib.util.spec_from_file_location("ios_lifecycle_qualify", path)
qualify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualify)


class LifecycleTests(unittest.TestCase):
    def lane(self, root):
        out, documents = root / "out", root / "Documents"
        out.mkdir()
        documents.mkdir()
        lane = qualify.Qualification(root / "Phux Rune.app", root / "phux", out, root)
        lane.udid = "OWNED-SIMULATOR"
        lane.require_alive = Mock()
        return lane, documents

    def launcher(self, documents, resumed_frames=8, marker=True):
        def launch(*args):
            self.assertEqual(args[:2], ("launch", "OWNED-SIMULATOR"))
            if args[2] == "com.apple.springboard":
                source = documents / "rune-background.json"
                self.assertFalse(source.exists(), "stale background receipt must be removed")
                source.write_text('{"schema":1,"background_frames":7}')
            else:
                self.assertEqual(args[2], qualify.BUNDLE_ID)
                source = documents / "rune-lifecycle.json"
                self.assertFalse(source.exists(), "stale resume receipt must be removed")
                source.write_text(json.dumps({"schema": 1, "background_frames": 7,
                                              "resumed_frames": resumed_frames, "marker_seen": marker}))
            return ""
        return launch

    def test_cycle_uses_running_home_and_requires_fresh_receipts_for_each_process(self):
        for prefix in ("", "resume-"):
            with self.subTest(prefix=prefix), tempfile.TemporaryDirectory() as temporary:
                lane, documents = self.lane(Path(temporary))
                for name in ("rune-background.json", "rune-lifecycle.json"):
                    (documents / name).write_text('{"stale":true}')
                lane.simctl = Mock(side_effect=self.launcher(documents))
                lane.cycle_scene(documents, 123, prefix)
                self.assertEqual(lane.simctl.call_args_list, [
                    call("launch", "OWNED-SIMULATOR", "com.apple.springboard"),
                    call("launch", "OWNED-SIMULATOR", qualify.BUNDLE_ID),
                ])
                lane.require_alive.assert_called_once_with(123)
                background = json.loads((lane.out / (prefix + "rune-background.json")).read_text())
                resumed = json.loads((lane.out / (prefix + "rune-lifecycle.json")).read_text())
                self.assertEqual(background["background_frames"], 7)
                self.assertEqual(resumed["resumed_frames"], 8)
                self.assertIs(resumed["marker_seen"], True)

    def test_home_without_real_background_acknowledgement_still_fails_within_existing_budget(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane, documents = self.lane(Path(temporary))
            (documents / "rune-background.json").write_text('{"background_frames":7}')
            lane.simctl = Mock(return_value="")
            with patch.object(qualify.time, "monotonic", side_effect=[0, 31]), \
                    self.assertRaisesRegex(TimeoutError, "acknowledge OS backgrounding"):
                lane.cycle_scene(documents, 123)
            self.assertFalse((documents / "rune-background.json").exists())
            lane.simctl.assert_called_once_with("launch", "OWNED-SIMULATOR", "com.apple.springboard")
            lane.require_alive.assert_called_once_with(123)

    def test_resume_still_requires_the_marker_and_an_advanced_presented_frame(self):
        for frames, marker in ((7, True), (8, False)):
            with self.subTest(frames=frames, marker=marker), tempfile.TemporaryDirectory() as temporary:
                lane, documents = self.lane(Path(temporary))
                lane.simctl = Mock(side_effect=self.launcher(documents, frames, marker))
                with self.assertRaisesRegex(ValueError, "same live terminal"):
                    lane.cycle_scene(documents, 123)
                self.assertFalse((lane.out / "rune-lifecycle.json").exists())

    def test_foreground_without_a_new_presented_frame_receipt_times_out(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane, documents = self.lane(Path(temporary))
            (documents / "rune-lifecycle.json").write_text('{"stale":true}')
            def launch(*args):
                if args[-1] == "com.apple.springboard":
                    (documents / "rune-background.json").write_text('{"background_frames":7}')
                return ""
            lane.simctl = Mock(side_effect=launch)
            with patch.object(qualify.time, "monotonic", side_effect=[0, 1, 32]), \
                    self.assertRaisesRegex(TimeoutError, "no presented frame"):
                lane.cycle_scene(documents, 123)
            self.assertEqual(lane.simctl.call_count, 2)
            self.assertEqual(lane.require_alive.call_count, 2)
            self.assertFalse((documents / "rune-lifecycle.json").exists())
            self.assertFalse((lane.out / "rune-lifecycle.json").exists())


if __name__ == "__main__":
    unittest.main()
