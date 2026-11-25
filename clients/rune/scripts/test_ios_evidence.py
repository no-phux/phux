"""Rendered selected-grid evidence must bind to the native readiness report."""

import contextlib
import copy
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

from test_ios_build import load, valid_report

qualify = load("qualify")
reports = load("qualification_reports")


def paired_evidence(marker=qualify.MARKER):
    report = {**valid_report(), "tree_marker_seen": True, "tree_frames": 3}
    tree = {
        "role": "window",
        "qualification": {"schema": 1, "marker": marker, "frames": report["frames"],
                          "terminal_id": report["terminal_id"]},
        "children": [
            {"role": "label", "name": qualify.SESSION},
            {"role": "generic", "key": "grid-" + report["terminal_id"],
             "children": [{"role": "label", "name": "[[" + marker + "]]"}]},
        ],
    }
    return tree, report


class EvidenceTests(unittest.TestCase):
    def test_live_report_cannot_make_session_only_tree_ready(self):
        _, report = paired_evidence()
        self.collect_invalid({"title": qualify.SESSION}, report)

    def collect_invalid(self, tree, report, resumed=False):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            documents = root / "Documents"
            documents.mkdir()
            (documents / "rune-qualification.json").write_text(json.dumps(report))
            (documents / "rune-tree.json").write_text(json.dumps(tree))
            lane = qualify.Qualification(root / "App", root / "phux", root, root)
            lane.documents = documents
            lane.require_alive = Mock()
            lane.cycle_scene = Mock()

            def screenshot(*args):
                Path(args[-1]).write_bytes(b"screenshot")

            lane.simctl = Mock(side_effect=screenshot)
            with patch.object(qualify.time, "sleep") as sleep, self.assertRaises(ValueError):
                lane.collect_report(123, resumed=resumed)
            lane.simctl.assert_not_called()
            lane.cycle_scene.assert_not_called()
            sleep.assert_not_called()

    def test_collector_rejects_old_phase_even_with_valid_resumed_report(self):
        tree, report = paired_evidence()
        report.update(onboarding_import=False, onboarding_confirmed=False, resumed_identity=True)
        self.collect_invalid(tree, report, resumed=True)

    def test_collector_rejects_resumed_tree_during_initial_phase(self):
        tree, report = paired_evidence(qualify.MARKER + "_RESUMED")
        self.collect_invalid(tree, report)

    def test_collector_accepts_current_resumed_pair_and_keeps_dwell_checks(self):
        tree, report = paired_evidence(qualify.MARKER + "_RESUMED")
        report.update(onboarding_import=False, onboarding_confirmed=False, resumed_identity=True)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            documents = root / "Documents"
            documents.mkdir()
            (documents / "rune-qualification.json").write_text(json.dumps(report))
            (documents / "rune-tree.json").write_text(json.dumps(tree))
            lane = qualify.Qualification(root / "App", root / "phux", root, root)
            lane.documents = documents
            lane.require_alive = Mock()
            lane.cycle_scene = Mock()
            lane.simctl = Mock(side_effect=lambda *args, **_: Path(args[-1]).write_bytes(b"screenshot"))
            with patch.object(qualify.time, "sleep") as sleep:
                self.assertEqual(lane.collect_report(123, resumed=True), report)
            sleep.assert_called_once_with(3)
            self.assertEqual(lane.require_alive.call_count, 2)
            lane.cycle_scene.assert_called_once_with(documents, 123, "resume-")
            self.assertEqual(json.loads((root / "resume-rune-tree.json").read_text()), tree)
            self.assertEqual(json.loads((root / "resume-rune-qualification.json").read_text()), report)
            self.assertTrue((root / "resume-ios-native.png").is_file())

    def test_valid_initial_and_resumed_pairs(self):
        for marker in (qualify.MARKER, qualify.MARKER + "_RESUMED"):
            with self.subTest(marker=marker):
                tree, report = paired_evidence(marker)
                reports.validate_tree(tree, report, marker)

    def test_marker_can_wrap_over_multiple_rows_or_styled_runs(self):
        for marker in (qualify.MARKER, qualify.MARKER + "_RESUMED"):
            tree, report = paired_evidence(marker)
            grid = tree["children"][1]
            witness = "[[" + marker + "]]"
            for split in range(1, len(witness)):
                with self.subTest(marker=marker, split=split):
                    grid["children"] = [
                        {"role": "generic", "children": [{"role": "label", "name": witness[:split]}]},
                        {"role": "generic", "children": [{"role": "label", "name": witness[split:]}]},
                    ]
                    reports.validate_tree(tree, report, marker)
                    grid["children"] = [{"role": "label", "name": witness[:split] + "\n" + witness[split:]}]
                    reports.validate_tree(tree, report, marker)

    def test_exact_marker_on_a_separate_row_is_not_hidden_by_flattening(self):
        tree, report = paired_evidence()
        tree["children"][1]["children"] = [
            {"role": "label", "name": "prefix"},
            {"role": "label", "name": "[[" + qualify.MARKER + "]]"},
            {"role": "label", "name": "suffix"},
        ]
        reports.validate_tree(tree, report, qualify.MARKER)

    def test_phase_marker_requires_closed_witness_boundaries(self):
        for marker in (qualify.MARKER, qualify.MARKER + "_RESUMED"):
            tree, report = paired_evidence(marker)
            for text in (marker + "_OLD", marker + "A", marker + "9", "A" + marker,
                         "9" + marker, "_" + marker):
                tree["children"][1]["children"][0]["name"] = "[[" + text + "]]"
                with self.subTest(marker=marker, text=text), self.assertRaises(ValueError):
                    reports.validate_tree(tree, report, marker)
            tree["children"][1]["children"][0]["name"] = f"prompt[[{marker}]]suffix"
            reports.validate_tree(tree, report, marker)

    def test_wrapped_wrong_phase_cannot_satisfy_current_phase(self):
        for current, stale in ((qualify.MARKER, qualify.MARKER + "_RESUMED"),
                               (qualify.MARKER + "_RESUMED", qualify.MARKER)):
            tree, report = paired_evidence(current)
            witness = "[[" + stale + "]]"
            for split in range(1, len(witness)):
                tree["children"][1]["children"] = [
                    {"role": "label", "name": witness[:split]},
                    {"role": "label", "name": witness[split:]},
                ]
                with self.subTest(current=current, split=split), self.assertRaises(ValueError):
                    reports.validate_tree(tree, report, current)

    def test_raw_marker_and_partial_witness_brackets_are_not_evidence(self):
        for marker in (qualify.MARKER, qualify.MARKER + "_RESUMED"):
            tree, report = paired_evidence(marker)
            for text in (marker, "[" + marker + "]", "[[" + marker, marker + "]]",
                         "[[" + marker + "]", "[" + marker + "]]"):
                tree["children"][1]["children"][0]["name"] = text
                with self.subTest(marker=marker, text=text), self.assertRaises(ValueError):
                    reports.validate_tree(tree, report, marker)

    def test_empty_marker_cannot_supply_an_empty_witness(self):
        tree, report = paired_evidence("")
        with self.assertRaises(ValueError):
            reports.validate_tree(tree, report, "")

    def test_stale_phase_marker_is_rejected_even_with_current_metadata(self):
        for current, stale in ((qualify.MARKER, qualify.MARKER + "_RESUMED"),
                               (qualify.MARKER + "_RESUMED", qualify.MARKER)):
            tree, report = paired_evidence(current)
            tree["children"][1]["children"][0]["name"] = "[[" + stale + "]]"
            with self.subTest(current=current), self.assertRaises(ValueError):
                reports.validate_tree(tree, report, current)

    def test_metadata_and_other_grid_cannot_supply_selected_grid_marker(self):
        tree, report = paired_evidence()
        tree["children"][1]["children"][0]["name"] = "old frame"
        tree["children"].append({"role": "generic", "key": "grid-other", "children": [
            {"role": "label", "name": "[[" + qualify.MARKER + "]]"}]})
        tree["name"] = "[[" + qualify.MARKER + "]]"
        tree["qualification"]["witness"] = "[[" + qualify.MARKER + "]]"
        with self.assertRaises(ValueError):
            reports.validate_tree(tree, report, qualify.MARKER)

    def test_grid_root_name_and_non_label_descendants_are_not_terminal_text(self):
        for role in ("generic", "button", "text-input"):
            tree, report = paired_evidence()
            grid = tree["children"][1]
            grid["name"] = "[[" + qualify.MARKER + "]]"
            grid["children"] = [{"role": role, "name": "[[" + qualify.MARKER + "]]"}]
            with self.subTest(role=role), self.assertRaises(ValueError):
                reports.validate_tree(tree, report, qualify.MARKER)

    def test_qualification_metadata_is_strictly_typed_and_report_bound(self):
        for field, values in {
            "schema": [None, True, "1", 2],
            "marker": [None, 1, qualify.MARKER + "_RESUMED"],
            "frames": [None, True, "3", 0, -1, 2, 4],
            "terminal_id": [None, 123, "other"],
        }.items():
            for value in values:
                tree, report = paired_evidence()
                tree["qualification"][field] = value
                with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                    reports.validate_tree(tree, report, qualify.MARKER)

    def test_missing_metadata_fields_are_rejected(self):
        original, report = paired_evidence()
        for field in original["qualification"]:
            tree = copy.deepcopy(original)
            del tree["qualification"][field]
            with self.subTest(field=field), self.assertRaises(ValueError):
                reports.validate_tree(tree, report, qualify.MARKER)

    def test_report_tree_readiness_is_strict_and_frame_bound(self):
        for field, values in {
            "tree_marker_seen": [None, False, 1, "true"],
            "tree_frames": [None, True, "3", 0, -1, 2, 4],
            "frames": [None, True, "3", 0, -1],
            "terminal_id": [None, 123, "", " "],
        }.items():
            for value in values:
                tree, report = paired_evidence()
                report[field] = value
                with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                    reports.validate_tree(tree, report, qualify.MARKER)

    def test_wrong_target_grid_and_missing_grid_are_rejected(self):
        for key in (None, "grid-other", "grid-123-extra"):
            tree, report = paired_evidence()
            tree["children"][1]["key"] = key
            with self.subTest(key=key), self.assertRaises(ValueError):
                reports.validate_tree(tree, report, qualify.MARKER)

    def test_duplicate_selected_grids_are_ambiguous_not_ready(self):
        tree, report = paired_evidence()
        tree["children"].append(copy.deepcopy(tree["children"][1]))
        with self.assertRaises(ValueError):
            reports.validate_tree(tree, report, qualify.MARKER)

    def test_truncated_or_malformed_tree_is_rejected(self):
        original, report = paired_evidence()
        for tree in (None, [], {}, {"qualification": original["qualification"]},
                     {**original, "children": None}, {**original, "children": [None]}):
            with self.subTest(tree=tree), self.assertRaises(ValueError):
                reports.validate_tree(tree, report, qualify.MARKER)

    def test_session_must_be_named_in_the_accessibility_tree(self):
        tree, report = paired_evidence()
        tree["children"][0] = {"role": "label", "description": qualify.SESSION}
        tree["unrelated"] = qualify.SESSION
        with self.assertRaises(ValueError):
            reports.validate_tree(tree, report, qualify.MARKER)
        tree["children"][0] = {"role": "label", "name": 123}
        with self.assertRaises(ValueError):
            reports.validate_tree(tree, report, qualify.MARKER)

    def test_missing_or_truncated_tree_file_fails_without_recapture(self):
        for contents in (None, '{"role":'):
            with self.subTest(contents=contents), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                documents = root / "Documents"
                documents.mkdir()
                _, report = paired_evidence()
                (documents / "rune-qualification.json").write_text(json.dumps(report))
                if contents is not None:
                    (documents / "rune-tree.json").write_text(contents)
                lane = qualify.Qualification(root / "App", root / "phux", root, root)
                lane.documents = documents
                lane.simctl = Mock()
                with patch.object(qualify.time, "sleep") as sleep, \
                        self.assertRaises((ValueError, OSError)):
                    lane.collect_report(123)
                lane.simctl.assert_not_called()
                sleep.assert_not_called()


class InputGateTimeoutTests(unittest.TestCase):
    """PHUX-834: the unchanged 90s timeout names a never-ready input gate."""

    def timeout_message(self, gate):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            documents = root / "Documents"
            documents.mkdir()
            if gate is not None:
                (documents / qualify.INPUT_GATE).write_text(json.dumps(gate))
            lane = qualify.Qualification(root / "App", root / "phux", root, root)
            lane.documents = documents
            lane.require_alive = Mock()
            lane.simctl = Mock()
            clock = iter((0.0, 45.0, 90.0))
            with patch.object(qualify.time, "monotonic", side_effect=lambda: next(clock)), \
                    patch.object(qualify.time, "sleep"), \
                    self.assertRaises(TimeoutError) as raised:
                lane.collect_report(123)
            lane.simctl.assert_not_called()
            return str(raised.exception)

    def test_a_report_written_during_a_slow_liveness_check_is_collected(self):
        # A liveness check that outlasts the budget must not expire it unseen.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            documents = root / "Documents"
            documents.mkdir()
            lane = qualify.Qualification(root / "App", root / "phux", root, root)
            lane.documents = documents
            now = [0.0]

            def slow_alive(pid):
                now[0] += 95.0
                (documents / "rune-qualification.json").write_text("{not yet validated")

            lane.require_alive = Mock(side_effect=slow_alive)
            with patch.object(qualify.time, "monotonic", side_effect=lambda: now[0]), \
                    patch.object(qualify.time, "sleep"), \
                    self.assertRaises(json.JSONDecodeError):
                lane.collect_report(123)
            lane.require_alive.assert_called_once_with(123)

    def test_a_shown_frame_that_never_became_input_ready_is_named(self):
        message = self.timeout_message({"schema": 1, "frame_shown": True, "input_ready": False})
        self.assertTrue(message.startswith("native qualification report not written within 90s"))
        self.assertIn("input_ready never became true", message)

    def test_other_timeouts_keep_the_original_message(self):
        original = "native qualification report not written within 90s"
        for gate in (None, {"schema": 1, "frame_shown": True, "input_ready": True}, ["junk"]):
            with self.subTest(gate=gate):
                self.assertEqual(self.timeout_message(gate), original)


class InputOrderLogTests(unittest.TestCase):
    """PHUX-834: the qualify log prints input_ready:true before the first InputKey."""

    def trace(self, rows):
        return "".join(json.dumps(row) + "\n" for row in rows)

    def row(self, phase, elapsed, process=None, connection=1, attempt=1):
        return {
            "connection": connection, "attempt": attempt, "elapsed_ms": elapsed,
            "process_elapsed_ms": elapsed if process is None else process, "phase": phase,
        }

    def test_input_ready_is_printed_before_the_first_input_key(self):
        text = self.trace([
            self.row("attempt_started", 0),
            self.row("input_injected", 1, 1),
            self.row("input_ready", 10, 40),
            self.row("input_injected", 12, 42),
            self.row("input_injected", 13, 43),
        ])
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            out = root / "out"
            out.mkdir()
            (out / "rune-connection-diagnostics.jsonl").write_text(text)
            (out / "resume-rune-connection-diagnostics.jsonl").write_text(self.trace([
                self.row("input_ready", 5, 5),
                self.row("input_injected", 6, 6),
            ]))
            lane = qualify.Qualification(root / "App", root / "phux", out, root, "wss")
            with contextlib.redirect_stdout(io.StringIO()) as printed:
                lane.emit_input_order()
        self.assertEqual(printed.getvalue().splitlines(), [
            "wss: input_ready input_ready:true elapsed_ms=10 process_elapsed_ms=40",
            "wss: input_injected InputKey elapsed_ms=12 process_elapsed_ms=42",
            "wss resume: input_ready input_ready:true elapsed_ms=5 process_elapsed_ms=5",
            "wss resume: input_injected InputKey elapsed_ms=6 process_elapsed_ms=6",
        ])

    def test_input_order_is_not_printed_without_a_later_injection(self):
        text = self.trace([
            self.row("input_injected", 1),
            self.row("input_ready", 2),
        ])
        with contextlib.redirect_stdout(io.StringIO()) as printed:
            qualify.print_input_order_lines("quic", text)
        self.assertEqual(printed.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
