"""Onboarding proof must import the QR payload and survive a ticket-free relaunch."""
import importlib.util
from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest.mock import patch

from test_ios_build import build, valid_report

path = Path(__file__).resolve().parents[1] / "ios/qualify.py"
spec = importlib.util.spec_from_file_location("onboarding_qualify", path)
qualify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualify)


def resumed_report():
    return {**valid_report(), "onboarding_import": False, "onboarding_confirmed": False,
            "resumed_identity": True}


class OnboardingTests(unittest.TestCase):
    def test_cleanup_preserves_native_failure_but_still_fails_a_successful_run(self):
        for native_failure in (None, TimeoutError("native launch timed out")):
            with self.subTest(native_failure=native_failure), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                app, phux, out = root / "App", root / "phux", root / "out"
                app.mkdir()
                phux.write_bytes(b"fixture")
                (app / "Info.plist").write_bytes(plistlib.dumps(build.bundle_plist("1.0", True, [])))
                with patch.object(qualify, "Qualification") as constructor, patch.object(qualify.sys, "stderr"):
                    lane = constructor.return_value
                    lane.launch.side_effect = native_failure
                    lane.launch.return_value = 42
                    lane.collect_report.return_value = valid_report()
                    lane.resume_identity.return_value = resumed_report()
                    lane.cleanup.side_effect = ValueError("owned simulator cleanup failed")
                    expected = (TimeoutError, "native launch timed out") if native_failure else (ValueError, "owned simulator cleanup failed")
                    with self.assertRaisesRegex(*expected):
                        qualify.qualify(app, phux, out)
                lane.cleanup.assert_called_once_with()
                self.assertEqual((out / "evidence.json").exists(), native_failure is None)

    def test_import_confirmation_and_key_tag_are_required(self):
        qualify.validate_report(valid_report())
        for field, value in [("onboarding_import", False), ("onboarding_confirmed", False),
                             ("resumed_identity", True), ("key_id", None), ("key_id", "not-a-uuid")]:
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                qualify.validate_report({**valid_report(), field: value})
        qualify.validate_report(resumed_report(), resumed=True)
        with self.assertRaises(ValueError):
            qualify.validate_report(valid_report(), resumed=True)

    def test_every_named_contract_group_is_present_once(self):
        groups = list(qualify.INPUT_CONTRACTS)
        for invalid in [groups[:-1], groups + [groups[0]], [str(n) for n in range(12)]]:
            with self.subTest(groups=invalid), self.assertRaises(ValueError):
                qualify.validate_input_contracts(invalid)

    def lane(self, root):
        lane = qualify.Qualification(root / "App", root / "dev-phux", root, root)
        lane.udid = "owned-simulator"
        lane.documents = root / "Documents"
        lane.documents.mkdir()
        for name in ["rune-qualification.json", "rune-tree.json"]:
            (lane.documents / name).write_text("stale")
        return lane

    def test_relaunch_has_no_ticket_or_pairing_payload_and_reuses_identity(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            with patch.object(lane, "simctl", side_effect=["", qualify.BUNDLE_ID + ": 42\n"]) as run, \
                    patch.object(lane, "collect_report", return_value=resumed_report()) as collect:
                lane.resume_identity(valid_report())
            self.assertEqual(run.call_args_list[0].args, ("terminate", "owned-simulator", qualify.BUNDLE_ID))
            launch = run.call_args_list[1].args
            self.assertIn("--qualification-resume", launch)
            self.assertNotIn("--qualification-configuration", launch)
            self.assertFalse(any("ticket" in str(arg) or "token=" in str(arg) for arg in launch))
            collect.assert_called_once_with(42, resumed=True)
            self.assertFalse((lane.documents / "rune-qualification.json").exists())

    def test_relaunch_cannot_silently_enroll_a_different_key(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            changed = {**resumed_report(), "key_id": "22222222-2222-4222-8222-222222222222"}
            with patch.object(lane, "simctl", side_effect=["", qualify.BUNDLE_ID + ": 42\n"]), \
                    patch.object(lane, "collect_report", return_value=changed), \
                    self.assertRaisesRegex(ValueError, "same saved device identity"):
                lane.resume_identity(valid_report())


if __name__ == "__main__":
    unittest.main()
