#!/usr/bin/env bash
# Execute every introductory example using the caller's same-checkout binary.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PHUX="${PHUX:-$ROOT/target/debug/phux}"
if [[ ! -x "$PHUX" ]]; then
    echo "build this checkout first (just build), or set PHUX=/absolute/path/to/phux" >&2
    exit 2
fi
export PYTHONDONTWRITEBYTECODE=1 PHUX_EXAMPLES_NATIVE=1
python3 "$ROOT/scripts/test-agent-examples.py"

for example in "$ROOT"/examples/agents/0[1-4]-*.sh; do
    echo "examples smoke: $(basename "$example")"
    bash "$example"
done
echo "examples smoke: agent_loop.py"
python3 "$ROOT/examples/agents/agent_loop.py"

# Example 03 proves an early wait misses the echoed command and a later wait
# sees actual output. Also check a real target failure is not called a timeout.
(
    # shellcheck source=examples/agents/lib.sh
    source "$ROOT/examples/agents/lib.sh"
    phux_start_server
    rc=0
    phux_expect_timeout --until missing --timeout 1 @999999 || rc=$?
    [[ "$rc" -ne 0 && "$rc" -ne 124 ]]
)
echo "examples smoke: all introductory examples and output-only wait verified"
