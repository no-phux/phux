#!/usr/bin/env bash
# demo-setup.sh — stage the product demo session (docs/demo.md).
#
# Creates a session named "demo" headlessly, then prints the recording
# runbook. The payload itself (docs/assets/payload.sh) is run from inside
# the attached pane during Beat 1, so the content is painted at the size
# you are recording at — not at the unattached default.
#
# Usage: scripts/demo-setup.sh
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Prefer a phux on PATH; fall back to local builds.
if command -v phux >/dev/null 2>&1; then
  PHUX=phux
elif [ -x "$REPO/target/release/phux" ]; then
  PHUX="$REPO/target/release/phux"
elif [ -x "$REPO/target/debug/phux" ]; then
  PHUX="$REPO/target/debug/phux"
else
  echo "no phux binary found: put one on PATH or run 'cargo build --bin phux'" >&2
  exit 1
fi

SESSION=demo

# Create-only; refuses a duplicate name, which is what we want — a stale
# "demo" session from a previous take should be killed, not reused.
if ! "$PHUX" new --json -s "$SESSION" >/dev/null 2>&1; then
  if "$PHUX" ls --json 2>/dev/null | grep -q "\"$SESSION\""; then
    echo "session \"$SESSION\" already exists; kill it first for a clean take:" >&2
    echo "  $PHUX kill $SESSION" >&2
    exit 1
  fi
  echo "could not create session \"$SESSION\" (is a server reachable?)" >&2
  exit 1
fi

cat <<EOF
Session "$SESSION" is up. The storyboard is docs/demo.md; the short version:

Beat 1 — it survives (record real pixels; asciinema cannot carry kitty
graphics). In a graphics-capable terminal (Ghostty, kitty, WezTerm),
start your screen recorder, then:

    $PHUX attach $SESSION
    bash docs/assets/payload.sh     # truecolor, styles, OSC 8, an image
    Ctrl-A d                        # detach; the server keeps the pane
    $PHUX attach $SESSION           # reattach — everything is still there

Beat 2 — an agent could have done that. From a second terminal, no TTY:

    $PHUX run $SESSION "cargo --version"
    $PHUX watch --json $SESSION     # live events; Ctrl-C to cut

Afterwards:

    $PHUX kill $SESSION
EOF
