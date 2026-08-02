---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-06-23
---

# Agent asked capture harness

**TL;DR.** The harness in `scripts/agent-asked-capture.sh` collects
clean-room evidence from locally installed agent CLIs by running them in
isolated phux panes and recording phux-owned watch and snapshot output.
It produces a local corpus for future detector design without making
passive detection authoritative.

## Purpose

ADR-0036 chooses explicit hook reports as the authoritative source for
agent `asked` events. Passive detection remains a fallback candidate,
but it needs a phux-owned empirical corpus before any manifests or
heuristics ship. This harness creates that corpus.

## Clean-Room Boundary

The harness does not read or copy herdr source, manifests, regular
expressions, hook scripts, or assets. It launches locally installed
agent CLIs and records observations through phux's own surfaces:

- `phux watch --json` for title, bell, dirty, idle, and asked events.
- `phux snapshot --json --scrollback` for visible grid rows and history.
- direct `<agent> --version` stdout/stderr for availability metadata.

The generated corpus is evidence only. It is not a detector and does
not change the source order from ADR-0036.

## Running It

```bash
scripts/agent-asked-capture.sh
scripts/agent-asked-capture.sh --prompt "Ask for approval before continuing"
PHUX=target/debug/phux scripts/agent-asked-capture.sh --out .omo/evidence/manual-asked-corpus
```

By default it probes `claude`, `codex`, and `pi`, writes under
`.omo/evidence/agent-asked-capture/<timestamp>`, and exits non-zero with
an incomplete-coverage report when fewer than two candidates are
available and exercised. Override the candidate list with
`--agents "claude codex pi"` or `PHUX_ASK_CAPTURE_AGENTS`.

## Output Shape

Each agent gets a directory containing:

- `metadata.json` with availability, command path, session name, dwell
  time, and whether a prompt was sent.
- `version.stdout`, `version.stderr`, and `version.status`.
- `watch.jsonl` and `watch.stderr`.
- `snapshot-start.json`, `snapshot-start.txt`, `snapshot-end.json`, and
  `snapshot-end.txt`.
- `new.json` / `new.stderr` and cleanup command output.

The top-level directory includes `summary.tsv`, `server.log`, and
`CLEAN_ROOM_NOTES.md`. When coverage is incomplete, it also includes
`INCOMPLETE_COVERAGE.txt`.
