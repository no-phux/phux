"""Wait, bounded, for a just-booted simulator's first-boot work to drain.

A simulator keeps doing first-boot work for minutes after `bootstatus -b`
returns. On CI runners, install, launch and launchctl calls in that window
take 20-130s, and the app's 18s connect budget runs out. Before installing,
wait until SETTLE_STREAK consecutive launchctl queries each answer within
SETTLE_PROMPT seconds, for at most SETTLE_BUDGET seconds.
"""

import json
import subprocess
import sys
import time

SETTLE_BUDGET = 300
SETTLE_PROMPT = 2.0
SETTLE_STREAK = 3


def settle(simctl, udid, out):
    """Never fatal: a simulator still slow at the budget proceeds under the
    per-command deadlines, and settle.json names the slow phase if one then
    fails. `simctl` is the lane's bounded simctl runner."""
    started = time.monotonic()
    samples, streak = [], 0
    while streak < SETTLE_STREAK and time.monotonic() - started < SETTLE_BUDGET:
        asked = time.monotonic()
        try:
            simctl("spawn", udid, "launchctl", "list")
            answered = time.monotonic() - asked
        except subprocess.TimeoutExpired:
            answered = None
        samples.append(answered)
        streak = streak + 1 if answered is not None and answered <= SETTLE_PROMPT else 0
        if streak < SETTLE_STREAK:
            time.sleep(1)
    elapsed = time.monotonic() - started
    settled = streak >= SETTLE_STREAK
    (out / "settle.json").write_text(json.dumps({
        "settled": settled, "elapsed_seconds": elapsed, "budget_seconds": SETTLE_BUDGET,
        "query_seconds": samples,
    }) + "\n")
    if not settled:
        print(f"simulator {udid} still slow after {elapsed:.0f}s of settling "
              f"(last queries: {samples[-SETTLE_STREAK:]}); continuing under command deadlines",
              file=sys.stderr)
    return settled
