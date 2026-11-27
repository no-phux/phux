"""Closed establishment vocabulary remains diagnostic, never readiness proof."""
import json
from pathlib import Path
import re
import unittest

from test_ios_diagnostics import diagnostics, trace_bytes


class DialStageDiagnostics(unittest.TestCase):
    def test_closed_rust_and_python_phase_vocabularies_match(self):
        source = Path(__file__).resolve().parents[1] / "ios/bridge/src/diagnostics.rs"
        literal = source.read_text().split("const PHASES: &[&str] = &[", 1)[1].split("];", 1)[0]
        self.assertEqual(set(re.findall(r'"([^"]+)"', literal)), diagnostics.PHASES)

    def test_establishment_stages_do_not_claim_inventory_or_input_readiness(self):
        phases = ("attempt_started", "dial_plan_started", "dial_plan_ready", "ws_resolve",
                  "ws_tcp_connect", "ws_tls_config", "ws_tls_handshake", "tls_sign_started",
                  "tls_sign_returned", "ws_upgrade", "ws_upgraded")
        data = b"".join(json.dumps({
            "connection": 1, "attempt": 1, "elapsed_ms": index,
            "process_elapsed_ms": index, "phase": phase,
        }).encode() + b"\n" for index, phase in enumerate(phases))
        self.assertEqual(diagnostics.validate_trace(data)["ready_attempts"], 0)
        with self.assertRaisesRegex(ValueError, "missing usable inventory"):
            diagnostics.validate_trace(data, require_ready=True)
        # Older traces need not contain any of the optional discriminator rows.
        self.assertEqual(diagnostics.validate_trace(trace_bytes(), require_ready=True)["ready_attempts"], 1)


if __name__ == "__main__":
    unittest.main()
