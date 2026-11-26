"""Native Liquid Glass qualification: the report contract and runner step."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

from test_ios_build import build, load

glass = load("liquid_glass")


def evidence():
    def control(id):
        return {"id": id, "frame": [8, 2, 44, 44], "enabled": True, "label": id, "shown": True}
    return {"schema": 1, "tapped": "phone-back", "terminal": [control(i) for i in glass.TOP_BAR],
            "sessions": [{**control(glass.DOCK), "frame": [16, 780, 361, 44]},
                         {**control(glass.DONE), "frame": [333, 60, 44, 44]}]}


class GlassReportTests(unittest.TestCase):
    def test_top_bar_then_sessions_dock_is_valid(self):
        glass.validate(evidence())

    def test_rejects_missing_covered_undersized_hidden_or_disabled_glass(self):
        def without_more(g):
            g["terminal"] = g["terminal"][:3]

        def bar_over_page(g):
            g["sessions"].append(g["terminal"][0])

        def small(g):
            g["terminal"][0]["frame"] = [0, 0, 30, 30]

        def hidden(g):
            g["terminal"][1]["shown"] = False

        def disabled_dock(g):
            g["sessions"][0]["enabled"] = False

        def untapped(g):
            g["tapped"] = None

        def no_done(g):
            g["sessions"] = g["sessions"][:1]

        for change in (without_more, bar_over_page, small, hidden, disabled_dock, untapped, no_done):
            report = evidence()
            change(report)
            with self.subTest(change.__name__), self.assertRaises(ValueError):
                glass.validate(report)


@unittest.skipUnless(sys.platform == "darwin", "Swift host execution needs Xcode")
class GlassProbeTests(unittest.TestCase):
    def test_actual_probe_waits_for_every_previous_control_not_only_back(self):
        source = Path(__file__).resolve().parents[1] / "ios/Sources/QualificationProbe.swift"
        # Execute the actual shipped method with inert evidence/write adapters.
        # UIKit, a simulator, hosting and device services are not involved.
        method = source.read_text().split("    private func recordGlass() throws {", 1)[1]
        method = "    private func recordGlass() throws {" + method.split(
            "    private static func readFixture", 1)[0]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            main = root / "main.swift"
            main.write_text("""
import Foundation
final class Surface { var glassEvidence: [[String: Any]] = [] }
final class Probe {
    var glassBefore: [[String: Any]]?
    var glassRecorded = false
    var surface: Surface? = Surface()
    var writes = 0
    func write(_ report: [String: Any], name: String) throws {
        precondition(name == "rune-glass.json")
        precondition(report["tapped"] as? String == "phone-back")
        writes += 1
    }
""" + method + """
    func frame() throws { try recordGlass() }
}
func controls(_ ids: [String]) -> [[String: Any]] { ids.map { ["id": $0] } }
let top = ["phone-back", "phone-place", "phone-news", "phone-more"]
let sessions = ["phone-session-new", "phone-navigation-close"]
let probe = Probe()
probe.glassBefore = controls(top)
// A moving drawer can uncover each old control in turn. None is ready,
// including the observed frame where Back is gone but News/More remain.
for remaining in [top, ["phone-news", "phone-more"]] + top.map({ [$0] }) {
    probe.surface!.glassEvidence = controls(sessions + remaining)
    try probe.frame()
    precondition(!probe.glassRecorded && probe.writes == 0,
                 "a partly covered top bar is not Sessions readiness")
}
for incomplete in [[], ["phone-session-new"], ["phone-navigation-close"]] {
    probe.surface!.glassEvidence = controls(incomplete)
    try probe.frame()
    precondition(!probe.glassRecorded && probe.writes == 0,
                 "the dock and Done must both be present")
}
probe.surface!.glassEvidence = controls(sessions)
try probe.frame()
precondition(probe.glassRecorded && probe.writes == 1)
try probe.frame()
precondition(probe.writes == 1, "readiness is published once")
""")
            compiled = subprocess.run(
                ["/usr/bin/xcrun", "--sdk", "macosx", "swiftc", "-swift-version", "6",
                 "-warnings-as-errors", str(main), "-o", str(root / "probe")],
                env=build.apple_environment(root), capture_output=True, text=True, timeout=120)
            self.assertEqual(compiled.returncode, 0, compiled.stderr)
            ran = subprocess.run([str(root / "probe")], capture_output=True, text=True, timeout=10)
            self.assertEqual(ran.returncode, 0, ran.stderr)


class GlassRunnerTests(unittest.TestCase):
    def lane(self, root, written):
        documents = root / "Documents"
        documents.mkdir()
        lane = Mock(documents=documents, out=root, udid="udid")

        def simctl(*args, **kwargs):
            if args[:3] == ("spawn", "udid", "notifyutil"):
                self.assertEqual(args[3:], ("-p", glass.SIGNAL))
                if written is not None:
                    (documents / "rune-glass.json").write_text(json.dumps(written))
            if args[0] == "io":
                Path(args[-1]).write_bytes(b"screenshot")
            return ""

        lane.simctl = Mock(side_effect=simctl)
        lane.await_scene_report = lambda source, pid, message: json.loads(source.read_text())
        return lane

    def test_posts_the_signal_then_keeps_report_and_screenshot(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lane = self.lane(root, evidence())
            with patch.object(glass.time, "sleep") as sleep:
                glass.qualify(lane, 123)
            sleep.assert_called_once_with(2)
            lane.require_alive.assert_called_once_with(123)
            self.assertTrue((root / "rune-glass.json").is_file())
            self.assertTrue((root / "ios-glass-sessions.png").is_file())

    def test_an_invalid_report_fails_before_the_screenshot(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bad = {**evidence(), "sessions": []}
            lane = self.lane(root, bad)
            with patch.object(glass.time, "sleep"), self.assertRaises(ValueError):
                glass.qualify(lane, 123)
            self.assertFalse((root / "ios-glass-sessions.png").exists())


if __name__ == "__main__":
    unittest.main()
