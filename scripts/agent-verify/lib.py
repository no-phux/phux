"""Public-surface helpers for the agent-state verification harness.

The server ranks evidence internally (declared, then stream, OSC 7501,
hook, process, screen) and does not publish that rank. These helpers name
the strongest tier `phux agent show --json` and `phux resource show --json`
can actually prove.
"""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

# What the public CLI can prove, strongest first. `declared` and `hook` are
# real rungs, but this surface cannot name them when they are the winner.
# `process` has no producer and no state field.
TIER_STREAM = "stream"
TIER_OSC = "osc7501"
TIER_SCREEN = "screen"
TIER_UNRESOLVED = "unresolved"

LIFECYCLE = ("unknown", "idle", "working", "blocked", "done")
WORKING_GLYPH = "◐"
DONE_GLYPH = "◆"

LIMITATIONS = [
    "phux.agent/v1 and agent_state watch events do not carry the server's evidence rank. The harness names the strongest tier those public documents can prove.",
    "A hook report (phux agent report-state) that agrees with the on-screen rule is reported as screen: agent show replays the manifest and describes the record as derived by it.",
    "An explicit phux agent set --state is not flagged in the public record. When the screen disagrees and neither a live session nor an OSC 7501 record accounts for the state, the tier is unresolved (declared or hook).",
    "The process rung has no producer and no public state field. resource show's process object is identity, not a lifecycle tier.",
    "agent_state lines from phux watch --json have no ts_ms. Time-to-state is the harness clock from the stimulus to the line the API client reads.",
    "phux attach draws a glyph, not the state word. working is ◐ and an unseen done is ◆. idle, unknown, and a seen done share ○ with other chrome, so those states are not compared by glyph.",
    "Codex has screen rules and no phux lifecycle integration. The integration-on row is integration_unavailable, not a measured session.",
]

_HOME_RE = re.compile(r"(?:/home|/Users)/[^\s\"']+")
_TOKEN_RE = re.compile(
    r"""(?ix)
    \bsk-(?:ant-)?[A-Za-z0-9_\-]{8,}
    |\bgh[pousr]_[A-Za-z0-9]{20,}
    |\bgithub_pat_[A-Za-z0-9_]{20,}
    |\bxox[baprs]-[A-Za-z0-9\-]{10,}
    |\bAKIA[0-9A-Z]{16}\b
    |\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}
    |\bBearer\s+[A-Za-z0-9._\-]{8,}
    """
)
_ASSIGN_RE = re.compile(
    r"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|secret|password|authorization)"
    r"(\s*[:=]\s*)([^\s,;\"']{6,})"
)


def redact(text: str, homes: list[str] | None = None) -> str:
    """Replace home directories and credential-shaped tokens."""
    if not text:
        return text
    out = text
    for home in homes or []:
        if home and home != "/":
            out = out.replace(home, "~")
    out = _HOME_RE.sub("~", out)
    out = _TOKEN_RE.sub("[redacted]", out)
    out = _ASSIGN_RE.sub(r"\1\2[redacted]", out)
    return out


def percentile(samples: list[int], pct: float) -> int | None:
    """Linear percentile. One sample is both p50 and max."""
    if not samples:
        return None
    ordered = sorted(samples)
    if len(ordered) == 1:
        return ordered[0]
    rank = (len(ordered) - 1) * (pct / 100.0)
    lo = int(rank)
    hi = min(lo + 1, len(ordered) - 1)
    frac = rank - lo
    return int(round(ordered[lo] * (1 - frac) + ordered[hi] * frac))


def _as_obj(value: Any) -> dict[str, Any]:
    return value if isinstance(value, dict) else {}


def unwrap_agent(document: dict[str, Any] | None) -> dict[str, Any]:
    """One agent object from `agent show --json` or `agent list --json`."""
    doc = _as_obj(document)
    agents = doc.get("agents")
    if isinstance(agents, list) and agents:
        first = agents[0]
        return first if isinstance(first, dict) else {}
    if "state" in doc:
        return doc
    return {}


def _stream_state(show: dict[str, Any], resource: dict[str, Any]) -> str | None:
    for source in show.get("sources") or []:
        if not isinstance(source, dict) or source.get("kind") != "stream":
            continue
        try:
            confidence = float(source.get("confidence") or 0)
        except (TypeError, ValueError):
            confidence = 0
        observed = source.get("observed")
        if confidence >= 0.99 and isinstance(observed, str) and observed in LIFECYCLE:
            return observed
    session = _as_obj(resource.get("agent_session"))
    state = session.get("state")
    if isinstance(state, str) and state not in ("", "unknown"):
        return state
    return None


def _osc_state(resource: dict[str, Any]) -> str | None:
    status = resource.get("program_status")
    if not isinstance(status, dict):
        return None
    active = status.get("active")
    if not isinstance(active, dict):
        return None
    state = active.get("state")
    if state == "error":
        return "done"
    if isinstance(state, str) and state in LIFECYCLE and state != "unknown":
        return state
    return None


def _live_session(show: dict[str, Any], resource: dict[str, Any]) -> bool:
    if _as_obj(show.get("agent_session")).get("resource"):
        return True
    session = resource.get("agent_session")
    return isinstance(session, dict) and bool(session)


def _screen_idle_fallback(show: dict[str, Any], state: str) -> bool:
    """A manifest that matches nothing publishes idle. That is still the screen rung."""
    if state != "idle":
        return False
    return any(
        isinstance(source, dict) and source.get("kind") == "detector_fallback"
        for source in show.get("sources") or []
    )


def _screen_agrees(show: dict[str, Any]) -> bool:
    explanation = str(show.get("explanation") or "")
    if "screen moved on" in explanation:
        return False
    if "derived by the" not in explanation or "manifest" not in explanation:
        return False
    return any(
        isinstance(source, dict) and source.get("kind") == "detector_rule"
        for source in show.get("sources") or []
    )


def classify(show_doc: dict[str, Any] | None, resource_doc: dict[str, Any] | None) -> dict[str, Any]:
    """Name the winning tier from one show document and one resource document."""
    show = unwrap_agent(show_doc)
    resource = _as_obj(resource_doc)
    state = show.get("state")
    if not isinstance(state, str) or state not in LIFECYCLE or state == "unknown":
        return {
            "tier": None,
            "state": state if isinstance(state, str) else None,
            "candidates": [],
            "note": "no published lifecycle state",
        }
    stream = _stream_state(show, resource)
    if stream == state:
        return {
            "tier": TIER_STREAM,
            "state": state,
            "candidates": [TIER_STREAM],
            "note": "agent show source kind stream agrees with the published state, or resource show agent_session.state does",
        }
    if not _live_session(show, resource):
        osc = _osc_state(resource)
        if osc == state:
            return {
                "tier": TIER_OSC,
                "state": state,
                "candidates": [TIER_OSC],
                "note": "resource show program_status.active matches the published state and no live agent session outranks it",
            }
    if _screen_agrees(show) or _screen_idle_fallback(show, state):
        return {
            "tier": TIER_SCREEN,
            "state": state,
            "candidates": [TIER_SCREEN],
            "note": "agent show says the record was derived by the detection manifest, or the manifest missed and the detector failed safe to idle. A hook report of the same state is not separately visible.",
        }
    return {
        "tier": TIER_UNRESOLVED,
        "state": state,
        "candidates": ["declared", "hook"],
        "note": "the published state is not accounted for by a live session, OSC 7501, or an agreeing screen rule",
    }


def markdown_table(runs: list[dict[str, Any]]) -> str:
    headers = (
        "agent",
        "integration",
        "tier",
        "time-to-state p50 (ms)",
        "time-to-state max (ms)",
        "two-client agreement",
        "result",
    )
    lines = [
        "| " + " | ".join(headers) + " |",
        "| " + " | ".join("---" for _ in headers) + " |",
    ]
    for run in runs:
        timing = run.get("time_to_state_ms") or {}
        cells = [
            _cell(run.get("agent")),
            _cell(run.get("integration")),
            _cell(run.get("tier")),
            _cell(timing.get("p50")),
            _cell(timing.get("max")),
            _cell(run.get("agreement")),
            _cell(run.get("result")),
        ]
        lines.append("| " + " | ".join(cells) + " |")
    return "\n".join(lines) + "\n"


def _cell(value: Any) -> str:
    if value is None or value == "":
        return "—"
    text = str(value).replace("|", "\\|").replace("\n", " ")
    return text


def write_results(out: Path, payload: dict[str, Any]) -> None:
    out.mkdir(parents=True, exist_ok=True)
    text = json.dumps(payload, indent=2, sort_keys=True) + "\n"
    (out / "results.json").write_text(text, encoding="utf-8")
    body = [
        "# Agent-state verification",
        "",
        markdown_table(payload.get("runs") or []),
        "",
        "## What could not be read from the public surface",
        "",
    ]
    for item in payload.get("limitations") or []:
        body.append(f"- {item}")
    body.append("")
    (out / "results.md").write_text("\n".join(body), encoding="utf-8")
