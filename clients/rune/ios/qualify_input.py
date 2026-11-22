"""Input-gate evidence for iOS qualification (PHUX-834).

The probe writes whether a shown frame was input-ready, and the connection
trace records that observation immediately before the first InputKey send.
This module turns those records into the timeout suffix and the CI log lines.
"""
import importlib.util
import json
from pathlib import Path

_spec = importlib.util.spec_from_file_location(
    "rune_connection_diagnostics", Path(__file__).with_name("connection_diagnostics.py"))
_trace = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_trace)

INPUT_GATE = "rune-input-gate.json"


def input_gate_reason(documents):
    """Name the probe's input gate when a shown terminal never took input."""
    try:
        gate = json.loads((documents / INPUT_GATE).read_text())
    except (OSError, ValueError):
        return ""
    if isinstance(gate, dict) and gate.get("frame_shown") is True and gate.get("input_ready") is False:
        return ("; a terminal frame was shown but input_ready never became true"
                " (the shown terminal's attach_terminal was never acknowledged)")
    return ""


def input_order_lines(transport, text):
    """The probe's input_ready row, then the first InputKey send after it."""
    ready = None
    injected = None
    for line in text.splitlines():
        if not line.strip():
            continue
        try:
            row = _trace.diagnostic_row(line)
        except ValueError:
            return []
        if ready is None:
            if row["phase"] == "input_ready":
                ready = row
            continue
        if row["phase"] != "input_injected":
            continue
        if (row["connection"], row["attempt"]) != (ready["connection"], ready["attempt"]):
            continue
        if row["elapsed_ms"] < ready["elapsed_ms"]:
            continue
        injected = row
        break
    if ready is None or injected is None:
        return []

    def stamped(row, label):
        return (f"{transport}: {label} elapsed_ms={row['elapsed_ms']} "
                f"process_elapsed_ms={row['process_elapsed_ms']}")

    return [
        stamped(ready, "input_ready input_ready:true"),
        stamped(injected, "input_injected InputKey"),
    ]


def print_input_order_lines(transport, text):
    for line in input_order_lines(transport, text):
        print(line, flush=True)
