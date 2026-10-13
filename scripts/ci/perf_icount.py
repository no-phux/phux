#!/usr/bin/env python3
"""Compare callgrind instruction counts to benchmarks/icount-baseline.json.

`scripts/ci/perf-icount.sh` runs the bench and calls this. The checked-in
file is the contract: a scenario fails when its instruction count rises by
more than `increase_limit_percent`. A drop passes. Missing or extra
scenarios fail. `--update` rewrites the scenario map from the measurement.

On a pull request, a change to the baseline file also needs the commit
trailer `Perf-Baseline: update` or `PHUX_PERF_BASELINE_UPDATE=1` (the
`perf-baseline` label). That check authorizes the edit; it does not skip
the percentage comparison.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BASELINE_PATH = ROOT / "benchmarks" / "icount-baseline.json"
TRAILER = "Perf-Baseline: update"
SCHEMA = {
    "schema": 1,
    "profile": "icount",
    "tool": "callgrind",
    "increase_limit_percent": 5,
}


def scenario_name(summary: dict) -> str:
    function = summary.get("function_name")
    if not isinstance(function, str) or not function:
        raise ValueError("benchmark summary has no function_name")
    ident = summary.get("id")
    if isinstance(ident, str) and ident:
        return f"{function}/{ident}"
    return function


def _int_metric(node) -> int:
    if isinstance(node, dict) and "Int" in node:
        return int(node["Int"])
    if isinstance(node, int):
        return node
    raise ValueError(f"instruction metric is not an integer: {node!r}")


def _metric_new(metrics) -> int:
    """The current run is Left, or the first element of Both."""
    if not isinstance(metrics, dict):
        raise ValueError(f"metrics object missing: {metrics!r}")
    if "Left" in metrics:
        return _int_metric(metrics["Left"])
    if "Both" in metrics:
        pair = metrics["Both"]
        if not isinstance(pair, list) or not pair:
            raise ValueError(f"Both metrics are empty: {metrics!r}")
        return _int_metric(pair[0])
    raise ValueError(f"no new instruction metric in {metrics!r}")


def instruction_count(summary: dict) -> int:
    profiles = summary.get("profiles")
    if not isinstance(profiles, list):
        raise ValueError("benchmark summary has no profiles list")
    for profile in profiles:
        if not isinstance(profile, dict):
            continue
        if profile.get("tool") not in (None, "Callgrind"):
            continue
        total = profile.get("summaries", {}).get("total", {})
        callgrind = total.get("summary", {}).get("Callgrind")
        if not isinstance(callgrind, dict) or "Ir" not in callgrind:
            continue
        return _metric_new(callgrind["Ir"].get("metrics"))
    keys = sorted(summary.keys())
    raise ValueError(f"no Callgrind Ir total in summary (keys={keys})")


def load_measurements(text: str) -> dict[str, int]:
    found: dict[str, int] = {}
    for line_no, line in enumerate(text.splitlines(), start=1):
        line = line.strip()
        if not line:
            continue
        try:
            summary = json.loads(line)
        except json.JSONDecodeError as exc:
            raise ValueError(f"iai json line {line_no} is not JSON: {exc}") from exc
        if not isinstance(summary, dict) or "function_name" not in summary:
            continue
        name = scenario_name(summary)
        if name in found:
            raise ValueError(f"duplicate scenario {name}")
        found[name] = instruction_count(summary)
    if not found:
        raise ValueError("iai output has no benchmark summaries")
    return found


def load_baseline(path: Path) -> dict:
    try:
        data = json.loads(path.read_text())
    except FileNotFoundError as exc:
        raise ValueError(
            f"missing {path}. Run `just perf-icount --update` on x86_64 "
            "inside nix develop and commit the file."
        ) from exc
    if data.get("schema") != 1:
        raise ValueError(f"{path} schema must be 1")
    if data.get("tool") != "callgrind" or data.get("profile") != "icount":
        raise ValueError(f"{path} must record the icount profile under callgrind")
    limit = data.get("increase_limit_percent")
    if not isinstance(limit, int) or isinstance(limit, bool) or limit < 0:
        raise ValueError("increase_limit_percent must be a non-negative integer")
    scenarios = data.get("scenarios")
    if not isinstance(scenarios, dict) or not scenarios:
        raise ValueError("baseline scenarios must be a non-empty object")
    parsed = {}
    for name, count in scenarios.items():
        if not isinstance(name, str) or not isinstance(count, int) or isinstance(count, bool):
            raise ValueError(f"scenario {name!r} must map to an integer instruction count")
        if count < 0:
            raise ValueError(f"scenario {name} has a negative count")
        parsed[name] = count
    data["scenarios"] = parsed
    return data


def over_limit(measured: int, baseline: int, limit_percent: int) -> bool:
    """True when the increase is strictly more than limit_percent."""
    return measured * 100 > baseline * (100 + limit_percent)


def compare(measured: dict[str, int], baseline: dict) -> list[str]:
    limit = baseline["increase_limit_percent"]
    expected = baseline["scenarios"]
    lines = []
    failures = []
    missing = sorted(set(expected) - set(measured))
    extra = sorted(set(measured) - set(expected))
    for name in missing:
        failures.append(f"FAIL {name}: missing from this run")
    for name in extra:
        failures.append(f"FAIL {name}: not in the baseline")
    for name in sorted(set(expected) & set(measured)):
        got = measured[name]
        want = expected[name]
        if want == 0:
            pct = "n/a"
            bad = got > 0
        else:
            pct = f"{(got - want) * 100 / want:+.2f}%"
            bad = over_limit(got, want, limit)
        status = "FAIL" if bad else "ok"
        lines.append(f"{status} {name}: measured {got} baseline {want} ({pct}, limit +{limit}%)")
        if bad:
            failures.append(lines[-1])
    print("\n".join(lines))
    return failures


def write_baseline(path: Path, measured: dict[str, int]) -> None:
    document = dict(SCHEMA)
    document["scenarios"] = {name: measured[name] for name in sorted(measured)}
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(document, indent=2) + "\n")
    print(f"wrote {path.relative_to(ROOT)}")


def trailer_present(message: str) -> bool:
    return any(line == TRAILER for line in message.splitlines())


def baseline_differs(base: str) -> bool:
    result = subprocess.run(
        ["git", "diff", "--quiet", f"{base}...HEAD", "--", "benchmarks/icount-baseline.json"],
        cwd=ROOT,
        check=False,
    )
    if result.returncode == 0:
        return False
    if result.returncode == 1:
        return True
    raise ValueError(
        f"git diff against {base} failed (exit {result.returncode}). "
        "Fetch the pull request base before running the guard."
    )


def authorize(base: str, event: str, label_update: str) -> None:
    if event != "pull_request":
        return
    if not base:
        raise ValueError("PHUX_BASE_SHA is required on pull_request")
    if not baseline_differs(base):
        return
    if label_update == "1":
        print("perf-icount: baseline change authorized by the perf-baseline label")
        return
    message = subprocess.check_output(
        ["git", "log", "--format=%B", f"{base}..HEAD"],
        cwd=ROOT,
        text=True,
    )
    if trailer_present(message):
        print("perf-icount: baseline change authorized by Perf-Baseline: update")
        return
    raise ValueError(
        "benchmarks/icount-baseline.json changed without authorization. "
        f"Add a commit whose message contains a line `{TRAILER}`, "
        "or push again after adding the `perf-baseline` label. "
        "The label is read on the next pull_request run; labeling alone "
        "does not start ci."
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)

    compare_cmd = sub.add_parser("compare")
    compare_cmd.add_argument("--measured", type=Path, required=True)
    compare_cmd.add_argument("--baseline", type=Path, default=BASELINE_PATH)

    update_cmd = sub.add_parser("update")
    update_cmd.add_argument("--measured", type=Path, required=True)
    update_cmd.add_argument("--baseline", type=Path, default=BASELINE_PATH)

    auth = sub.add_parser("authorize")
    auth.add_argument("--base", required=True)
    auth.add_argument("--event", default="")
    auth.add_argument("--label-update", default="0")

    args = parser.parse_args(argv)
    try:
        if args.cmd == "authorize":
            authorize(args.base, args.event, args.label_update)
            return 0
        measured = load_measurements(args.measured.read_text())
        if args.cmd == "update":
            write_baseline(args.baseline, measured)
            return 0
        baseline = load_baseline(args.baseline)
        failures = compare(measured, baseline)
    except (OSError, ValueError) as exc:
        print(f"perf-icount: {exc}", file=sys.stderr)
        return 1
    if failures:
        print(
            "perf-icount: instruction count rose more than the limit, "
            "or the scenario set does not match. See docs/performance-ci.md.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
