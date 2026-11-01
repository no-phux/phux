#!/usr/bin/env python3
"""Fast self-test: classifier, redaction, and a live run against the stub agent.

No real agent CLI and no credentials. CI runs this; it does not produce the
Mac numbers.
"""

from __future__ import annotations

import os
import shutil
import stat
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import lib  # noqa: E402
import run  # noqa: E402

HERE = Path(__file__).resolve().parent


def expect(cond: bool, message: str) -> None:
    if not cond:
        raise SystemExit(message)


def test_redact() -> None:
    home = "/home/operator"
    text = f"token sk-ant-api03-supersecret home {home}/.claude bearer Bearer abcdefghijklmnop"
    cleaned = lib.redact(text, [home])
    expect("supersecret" not in cleaned, cleaned)
    expect(home not in cleaned, cleaned)
    expect("/home/" not in cleaned, cleaned)
    expect("[redacted]" in cleaned, cleaned)
    expect("~" in cleaned, cleaned)


def test_percentile() -> None:
    expect(lib.percentile([], 50) is None, "empty")
    expect(lib.percentile([40], 50) == 40, "one sample")
    expect(lib.percentile([10, 30], 50) == 20, "midpoint")
    expect(lib.percentile([10, 20, 30, 40], 100) == 40, "max rank")


def test_classify() -> None:
    working = {
        "agents": [
            {
                "state": "working",
                "explanation": "agent session derived the state from its event stream",
                "sources": [{"kind": "stream", "confidence": 1, "observed": "working"}],
                "agent_session": {"resource": "@4", "provider": "claude"},
            }
        ]
    }
    resource = {"agent_session": {"provider": "claude", "state": "working"}, "program_status": {"active": {"state": "working"}}}
    ranked = lib.classify(working, resource)
    expect(ranked["tier"] == "stream", str(ranked))

    screen = {
        "agents": [
            {
                "state": "working",
                "explanation": "the phux.agent/v1 record for this pane, derived by the `claude` manifest: rule `title-busy-spinner` matched",
                "sources": [
                    {"kind": "agent_record", "confidence": 1, "observed": "{}"},
                    {"kind": "detector_rule", "confidence": 0.9, "observed": "line-regex"},
                ],
            }
        ]
    }
    ranked = lib.classify(screen, {"program_status": None, "agent_session": None})
    expect(ranked["tier"] == "screen", str(ranked))

    osc = {
        "agents": [
            {
                "state": "working",
                "explanation": "the phux.agent/v1 record for this pane",
                "sources": [{"kind": "agent_record", "confidence": 1, "observed": "{}"}],
            }
        ]
    }
    ranked = lib.classify(osc, {"program_status": {"active": {"state": "working", "app": "claude"}}})
    expect(ranked["tier"] == "osc7501", str(ranked))

    moved = {
        "agents": [
            {
                "state": "working",
                "explanation": "the screen moved on after the record was published",
                "sources": [{"kind": "agent_record", "confidence": 1, "observed": "{}"}],
            }
        ]
    }
    ranked = lib.classify(moved, {})
    expect(ranked["tier"] == "unresolved", str(ranked))
    expect(ranked["candidates"] == ["declared", "hook"], str(ranked))

    idle = {
        "agents": [
            {
                "state": "idle",
                "explanation": "nothing in the `claude` manifest matches this screen now",
                "sources": [{"kind": "detector_fallback", "confidence": 0.3, "observed": "no rule"}],
            }
        ]
    }
    ranked = lib.classify(idle, {})
    expect(ranked["tier"] == "screen", str(ranked))

    # A live session outranks OSC even when the screen rule also matches.
    both = {
        "agents": [
            {
                "state": "done",
                "explanation": "derived by the `claude` manifest: rule `title-busy-spinner` matched the title region",
                "sources": [
                    {"kind": "stream", "confidence": 1, "observed": "done"},
                    {"kind": "detector_rule", "confidence": 0.9, "observed": "x"},
                ],
                "agent_session": {"resource": "@4"},
            }
        ]
    }
    ranked = lib.classify(both, {"agent_session": {"state": "done"}, "program_status": {"active": {"state": "working"}}})
    expect(ranked["tier"] == "stream", str(ranked))


def test_table_marks_blanks() -> None:
    text = lib.markdown_table(
        [
            {
                "agent": "opencode",
                "integration": "n/a",
                "tier": None,
                "time_to_state_ms": {"p50": None, "max": None},
                "agreement": None,
                "result": "not_installed",
            }
        ]
    )
    expect("not_installed" in text, text)
    expect("| opencode | n/a | — | — | — | — | not_installed |" in text, text)


def test_live(phux: Path) -> None:
    stub = HERE / "stub_agent.py"
    mode = stub.stat().st_mode
    stub.chmod(mode | stat.S_IEXEC)
    with tempfile.TemporaryDirectory(prefix="phux-agent-verify-") as raw:
        root = Path(raw)
        bindir = root / "bin"
        home = root / "home"
        out = root / "out"
        bindir.mkdir()
        home.mkdir()
        for name in ("claude", "codex", "pi"):
            os.symlink(stub, bindir / name)
        saved = {key: os.environ.get(key) for key in ("PATH", "HOME", "XDG_CONFIG_HOME", "PHUX_SOCKET", "PHUX_PROFILE")}
        os.environ["PATH"] = f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin"
        os.environ["HOME"] = str(home)
        os.environ.pop("XDG_CONFIG_HOME", None)
        os.environ.pop("PHUX_SOCKET", None)
        os.environ.pop("PHUX_PROFILE", None)
        try:
            code = run.execute(
                ["--phux", str(phux), "--out", str(out), "--timeout", "25", "--home", str(home)]
            )
        finally:
            for key, value in saved.items():
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value
        results = out / "results.json"
        expect(results.is_file(), "results.json missing")
        payload = __import__("json").loads(results.read_text(encoding="utf-8"))
        by_key = {(row["agent"], row["integration"]): row for row in payload["runs"]}
        expect(by_key[("opencode", "n/a")]["result"] == "not_installed", str(by_key.get(("opencode", "n/a"))))
        expect(by_key[("codex", "on")]["result"] == "integration_unavailable", str(by_key[("codex", "on")]))
        expect(by_key[("codex", "on")]["tier"] is None, "codex on must not invent a tier")
        for agent, integration, tier in (
            ("claude", "off", "screen"),
            ("claude", "on", "stream"),
            ("codex", "off", "screen"),
            ("pi", "off", "screen"),
            ("pi", "on", "stream"),
        ):
            row = by_key[(agent, integration)]
            expect(row["result"] == "pass", f"{agent} {integration}: {row}")
            expect(row["tier"] == tier, f"{agent} {integration} tier {row['tier']}: {row.get('tier_note')}")
            expect(row["agreement"] == "yes", f"{agent} {integration} agreement {row.get('divergence')}")
            timing = row["time_to_state_ms"]
            expect(isinstance(timing["p50"], int) and timing["p50"] >= 0, str(timing))
            expect(isinstance(timing["max"], int) and timing["max"] >= timing["p50"], str(timing))
        blob = (out / "results.md").read_text(encoding="utf-8")
        expect("not_installed" in blob, blob)
        expect("integration_unavailable" in blob, blob)
        raw_logs = "\n".join(path.read_text(encoding="utf-8", errors="replace") for path in out.rglob("*") if path.is_file())
        expect(str(home) not in raw_logs, "home path leaked into results")
        expect("sk-" not in raw_logs, "token-shaped text leaked")
        expect(code == 0, f"harness exit {code}")


def main() -> None:
    test_redact()
    test_percentile()
    test_classify()
    test_table_marks_blanks()
    phux = os.environ.get("PHUX") or shutil.which("phux")
    if not phux:
        debug = HERE.parents[1] / "target" / "debug" / "phux"
        if debug.is_file():
            phux = str(debug)
    if not phux:
        raise SystemExit("self-test needs PHUX or target/debug/phux")
    test_live(Path(phux))
    print("agent-verify self-test: ok")


if __name__ == "__main__":
    main()
