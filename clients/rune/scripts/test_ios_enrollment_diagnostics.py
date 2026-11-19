"""Enrollment failures share strict capture, never consumer readiness."""
import json
from pathlib import Path
import tempfile
import unittest

import test_ios_diagnostics as connection

diagnostics = connection.diagnostics


class EnrollmentDiagnostics(unittest.TestCase):
    def test_a_later_consumer_cannot_erase_an_enrollment_failure(self):
        row = json.loads(connection.trace_bytes().splitlines()[0])
        failed = [
            {**row, "phase": "attempt_started"},
            {**row, "phase": "enroll_failed", "elapsed_ms": 1, "process_elapsed_ms": 1},
        ]
        consumer = [json.loads(line) for line in connection.trace_bytes(2).splitlines()]
        for event in consumer:
            event["process_elapsed_ms"] += 2
        data = b"".join(json.dumps(event).encode() + b"\n" for event in failed + consumer)
        self.assertEqual(diagnostics.validate_trace(data)["enrollment_failures"], 1)
        with self.assertRaisesRegex(ValueError, "cannot be erased"):
            diagnostics.validate_trace(data, require_ready=True)

    def test_shortcut_requires_the_exact_post_seal_failure_report(self):
        variants = [
            {}, {"schema": 1, "error": "", "preparation_failed": True},
            {"schema": True, "error": "failure", "preparation_failed": True},
            {"schema": 1, "error": "failure", "preparation_failed": 1},
            {"schema": 1, "error": "failure", "preparation_failed": True, "frames": 1},
        ]
        with tempfile.TemporaryDirectory() as temporary:
            fixture = connection.ConnectionDiagnostics()
            lane = fixture.lane(Path(temporary))
            fixture.seed(lane)
            stem = "rune-connection-diagnostics"
            report = lane.out / "rune-qualification.json"
            for value in variants:
                with self.subTest(value=value):
                    report.write_text(json.dumps(value))
                    self.assertFalse(diagnostics.preparation_failure_sealed(lane, "", stem))
            report.write_text('{"schema":1,"error":"failure","preparation_failed":true,'
                              '"preparation_failed":true}')
            self.assertFalse(diagnostics.preparation_failure_sealed(lane, "", stem))
            report.write_text('{"schema":1,"error":"failure","preparation_failed":true}')
            self.assertTrue(diagnostics.preparation_failure_sealed(lane, "", stem))
            (lane.documents / (stem + ".status.json")).unlink()
            self.assertFalse(diagnostics.preparation_failure_sealed(lane, "", stem))


if __name__ == "__main__":
    unittest.main()
