#!/usr/bin/env python3
"""Per-hop p50/p99 from a PHUX_ECHO_HOPS trace.

Lines are `pid <TAB> hop <TAB> unix_nanos`. One quiet pty-echo run is a
sequence of keystrokes separated by a gap. Hops, in path order:

    stdin send server_recv lane pty_write pty_read main frame_send client_recv stdout

`lane` is absent when the receive task wrote the PTY itself. Each sample is
the delta from the previous hop that did occur.

A keystroke starts at `stdin` and ends at the first `stdout`. The probe
writes Ctrl-U immediately after the echoed letter, and that control byte
still walks the input lane, so a time-gap group would pair the letter's
`pty_write` with the control byte's later `lane` stamp.
"""

from __future__ import annotations

import sys
from pathlib import Path

ORDER = (
    "stdin",
    "send",
    "server_recv",
    "lane",
    "pty_write",
    "pty_read",
    "main",
    "frame_send",
    "client_recv",
    "stdout",
)
def percentile(values: list[int], pct: float) -> int:
    if not values:
        return 0
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, round((pct / 100) * (len(ordered) - 1))))
    return ordered[index]


def load(path: Path) -> list[tuple[str, int]]:
    events: list[tuple[str, int]] = []
    for line in path.read_text(errors="replace").splitlines():
        parts = line.split("\t")
        if len(parts) != 3 or parts[1] not in ORDER:
            continue
        events.append((parts[1], int(parts[2])))
    events.sort(key=lambda item: item[1])
    return events


def groups(events: list[tuple[str, int]]) -> list[list[tuple[str, int]]]:
    """One group per `stdin`, closed at the first following `stdout`."""
    out: list[list[tuple[str, int]]] = []
    current: list[tuple[str, int]] = []
    for event in events:
        if event[0] == "stdin":
            if current:
                out.append(current)
            current = [event]
            continue
        if not current:
            continue
        current.append(event)
        if event[0] == "stdout":
            out.append(current)
            current = []
    if current:
        out.append(current)
    return out


def deltas(group: list[tuple[str, int]]) -> list[tuple[str, int]]:
    seen: dict[str, int] = {}
    for hop, nanos in group:
        seen.setdefault(hop, nanos)
    if "stdin" not in seen or "stdout" not in seen:
        return []
    present = [hop for hop in ORDER if hop in seen]
    samples: list[tuple[str, int]] = []
    for earlier, later in zip(present, present[1:]):
        samples.append((f"{earlier}->{later}", seen[later] - seen[earlier]))
    samples.append(("stdin->stdout", seen["stdout"] - seen["stdin"]))
    return samples


def main() -> int:
    path = Path(sys.argv[1] if len(sys.argv) > 1 else "/tmp/echo-hops.log")
    buckets: dict[str, list[int]] = {}
    used = 0
    for group in groups(load(path)):
        rows = deltas(group)
        if not rows:
            continue
        used += 1
        for edge, delta in rows:
            buckets.setdefault(edge, []).append(delta)
    print(f"keystrokes {used}")
    print(f"{'hop':<32} {'n':>6} {'p50_us':>8} {'p99_us':>8}")
    for edge in sorted(buckets, key=edge_key):
        values = buckets[edge]
        p50 = percentile(values, 50) // 1000
        p99 = percentile(values, 99) // 1000
        print(f"{edge:<32} {len(values):>6} {p50:>8} {p99:>8}")
    return 0


def edge_key(edge: str) -> tuple[int, int, int]:
    earlier, later = edge.split("->")
    # The end-to-end row stays first; the rest follow path order.
    total = 0 if edge == "stdin->stdout" else 1
    return (total, ORDER.index(earlier), ORDER.index(later))


if __name__ == "__main__":
    sys.exit(main())
