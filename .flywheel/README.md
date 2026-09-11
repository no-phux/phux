# .flywheel/

Local agent state. Two files matter:

- **`STOP`** — the kill switch. If this file exists, no agent in this repo runs.
  `tools/flywheel/guard.sh stop "reason"` creates it, `resume` clears it.
  A fleet-wide switch lives at `$FLYWHEEL_HOME/STOP` (default `~/.flywheel/STOP`)
  and takes precedence. Both are checked at the top of every agent run.
  Not committed — stopping the fleet must not require a push.

- **`agent-log.jsonl`** — append-only audit of what agents did (ADR 0003).
  Committed, with a union merge driver so parallel agents do not conflict.
  This is the greppable local copy; blackbird's event journal is the
  authenticated, tamper-evident record (ADR 0004).
