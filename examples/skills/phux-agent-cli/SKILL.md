---
name: phux-agent-cli
description: Drive phux as the canonical agent orchestration surface: create sessions, launch or spawn explicitly placed panes, reshape existing-pane layout, observe bounded events, report asks, and use the matching MCP tools without taking human focus or inventing lifecycle guarantees.
---

# phux agent orchestration: the canonical CLI and MCP loop

Use this skill when an agent needs to create and supervise a small terminal
fleet while a human may be attached to the same session. The CLI is the stable
contract (ADR-0022); `phux-mcp` is a strict JSON-RPC adapter over that same
surface. There is no separate orchestration service and no client SDK required.

Complete executable examples live at
[`orchestrate-placed-fleet`](../../agents/orchestrate-placed-fleet) for CLI and
[`orchestrate-placed-fleet-mcp.py`](../../agents/orchestrate-placed-fleet-mcp.py)
for JSON-RPC tool calls. The deterministic fake-phux gate is
`just agents-fleet-smoke`; real isolated dogfood is `just agents-fleet-live`.

To expose the same surface to Claude Code, install phux so `phux-mcp` is on
`PATH`, start phux, then register the stdio adapter exactly once:

```sh
claude mcp add phux -- phux-mcp
```

Other MCP hosts use the `phux-mcp` executable with stdio transport; registration
does not start the phux server.

## Invariants: keep the human and the wire honest

1. **Topology may be shared; focus is client-local.** Placement and
   `insert-pane` / `move-pane` / `swap-pane` write the persisted layout.
   They do not move an attached human's focus. Never write serialized focus or
   claim a headless focus operation.
2. **Attention is advisory.** `phux ask` raises the existing `asked` event.
   Tell the human to use `C-a q` to cycle asking panes and `C-a Q` to return.
   The orchestrator never presses those keys or moves focus for them.
3. **Every wait is bounded.** Give `phux wait` a finite `--timeout`. A CLI
   `watch` is a stream, so run it under a child-process deadline and reap it.
   For MCP, pass `timeout_secs` and/or `max_events` to `phux_watch`.
4. **Input is real input.** `send-keys` sends named keys or literal characters
   to a live PTY. It is not a clipboard abstraction. Prefer `run` for a
   discrete shell command and reserve `send-keys` for interactive programs.
5. **Destructive operations require an explicit target and confirmation.**
   Show the resolved pane/session and intended signal or kill to the human,
   receive affirmative confirmation, then invoke it. MCP destructive signals
   additionally require `confirm: true`.
6. **Do not model connection-scoped input authority as persistent agent state.**
   Short-lived CLI/MCP calls do not provide a durable take/give lease.
7. **Do not invent credentials, schedules, retries, or background ownership.**
   This skill assumes the caller already has local socket access. Shell child
   processes used to bound watches are observation mechanics, not a scheduler.

## 1. Discover and choose explicit identities

Start from machine-readable inventory and configured integrations:

```sh
phux ls --json
phux launch --list --json
phux workspace inspect --json .
```

Use exact local pane selectors (`@42`) once a command returns an id. Session or
window selectors may narrow to a selected pane, but placement and existing-pane
layout verbs require exact local, same-session panes. Headless `=` is rejected
because only an attached TUI owns previous-focus history. Satellite ids use
`host/@N`; satellite and local layout trees are not combined.

Flags belong to the verb, before trailing command/key arguments:

```sh
phux run --json --timeout 120 @42 "cargo test"
phux send-keys --socket /path/phux.sock @42 "continue" Enter
```

## 2. Create a session and place the fleet

Create-only `new --json` returns the seed pane without attaching:

```sh
seed_json=$(phux new --json -s review-fleet -c "$PWD")
# parse .terminal_id, then address it as @N
```

Use `launch` for a configured agent integration. It resolves an argv template,
starts the pane through the integration's identity wrapper, and returns the
pane id. Inspect with `--print`/`--dry-run` before spawning when needed.

```sh
phux launch codex --print --json -c "$PWD"
phux launch codex --json --target @10 --split vertical --ratio 0.55 -c "$PWD"
phux launch claude --json --target @11 --split horizontal --ratio 0.50 -c "$PWD"
```

Use `spawn` for explicit argv rather than an integration:

```sh
phux spawn --json --target @10 --split horizontal --ratio 0.70 \
  -c "$PWD" -- sh -lc 'exec make watch'
```

Direction names describe the divider the user sees:

- `vertical` = vertical divider = side-by-side panes;
- `horizontal` = horizontal divider = stacked panes (the default).

Ratios are finite values strictly between zero and one and are the fraction
retained by `--target`. `--target` placement is local-only. An unplaced spawn
keeps the legacy "most recently active session" behavior; prefer explicit
placement for orchestration.

## 3. Reshape terminals that already exist

These verbs mutate topology only and emit versioned JSON with user-facing
direction labels:

```sh
# NEW_PANE already exists but is not in the persisted tree
phux insert-pane @10 @14 --split vertical --ratio 0.4 --json

# collapse SOURCE, then insert it beside TARGET
phux move-pane @12 @14 --split horizontal --ratio 0.5 --json

# exchange leaf positions without changing split geometry
phux swap-pane @11 @12 --json
```

All selectors must resolve to distinct exact local panes. `insert-pane` and
`swap-pane` require one session; `move-pane` may cross sessions without
restarting the moved pane. `insert-pane` never spawns; `move-pane` never
clones; `swap-pane` preserves split geometry. None is a focus command.
Concurrent layout writers are
last-write-wins, so serialize edits in one orchestration process and re-read
state after a contested update.

## 4. Run work, then observe with hard bounds

For one command with a completion boundary, use `run`:

```sh
set +e
result=$(phux run --json --timeout 300 @11 "cargo test")
status=$?
set -e
```

On completion, JSON contains `command`, `exit_code`, `output`, `duration_ms`,
and `truncated`. The process mirrors the child exit code. Exit `125` means phux
itself timed out and emits no completion JSON. Treat nonzero child codes as
results, not subprocess-wrapper exceptions.

For a known screen condition, always bound `wait`:

```sh
phux wait --until "test result:" --timeout 300 @11
phux snapshot --json --scrollback 200 @11
```

`wait --until` sees visible command echo as well as output. Match text produced
only by the result, not a literal already present in the typed command. Timeout
is exit `124`.

For pushed lifecycle/activity/ask events, run independent bounded watchers so
one quiet pane cannot block collection from the others:

```sh
phux watch --json @11 >builder.jsonl & builder_watch=$!
phux watch --json @12 >reviewer.jsonl & reviewer_watch=$!
( sleep 30; kill "$builder_watch" "$reviewer_watch" 2>/dev/null || true ) &
# reap both watcher pids before the orchestration process exits
```

A CLI watch runs until EOF, Ctrl-C, or your process bound. It does not have a
CLI timeout flag. JSONL events include `asked`, title, bell, dirty/idle, and
pane lifecycle events. Command-boundary event tags exist, but current server
emission remains incomplete; do not build correctness on them. Use `run` or a
bounded `wait` for completion.

## 5. Surface a blocked ask; do not seize attention

An integration reports a human-answerable block on its own pane:

```sh
phux ask @11 --id approve-deploy --suggest yes --suggest no \
  --elapsed-seconds 30 --json "Approve deployment?"
```

The server emits the same `asked` event consumed by `phux watch`, TUI badges,
and the agent-fleet dashboard. Present the question, suggestions, terminal id,
and elapsed time to the human. Then print guidance such as:

```text
Attach to review-fleet. Press C-a q to visit the next asking pane;
press C-a Q once to return to where attention navigation began.
```

Do not send those chords through `send-keys`: they belong to the human's local
TUI. Merely visiting a pane does not clear its ask; attention clears when input
is forwarded to that pane.

## 6. MCP parity

`phux-mcp` exposes 21 strict tools. The orchestration mappings are direct:

| CLI | MCP tool | Bound/safety rule |
|---|---|---|
| `ls --json` | `phux_ls` | inventory read |
| `snapshot --json` | `phux_snapshot` | side-effect-free read |
| `run --json` | `phux_run` | `timeout_secs` is bounded to 1–3600 |
| `wait` | `phux_wait` | always supply `timeout_secs` |
| `watch --json` | `phux_watch` | supply `timeout_secs` and/or `max_events` |
| `new --json` | `phux_new` | create-only, no attach |
| `launch --json` | `phux_launch` | optional exact placement |
| `spawn --json` | `phux_spawn` | optional exact placement or satellite |
| `ask --json` | `phux_ask` | same advisory `asked` event |
| spatial verbs | `phux_insert_pane`, `phux_move_pane`, `phux_swap_pane` | exact local same-session panes |
| `signal` | `phux_signal` | destructive signals require `confirm: true` |
| `agent` | `phux_agent` | identity/state projection and record updates |
| `workspace` | `phux_workspace` | inspect/save/restore only |

The remaining tools cover send-keys, kill, tags, rename, plugin actions, and
plugin workspace profiles. `tools/list` is authoritative. Every parity schema
rejects unknown properties, invokes argv directly rather than through a shell,
and returns canonical CLI JSON or a documented small projection.

MCP has no headless focus tool and deliberately has no durable input-authority
tool. It also does not accept remote credentials or mutate satellite trust.

## 7. Destructive closeout

Before `kill`, `terminate`, or an interrupt that may lose work:

1. resolve and display the exact selector (`@N` preferred);
2. snapshot relevant output/state;
3. state what will be lost;
4. obtain affirmative human confirmation;
5. issue the narrowest operation;
6. verify the pane/session lifecycle event or inventory change under a bound.

Example only after confirmation:

```sh
phux kill @12
# MCP: phux_signal { target: "@12", signal: "terminate", confirm: true }
```

Do not interpret a watcher process ending as proof that work completed. Re-read
`phux ls --json`, `phux agent list --json`, or a targeted snapshot.

## 8. Canonical loop

```text
DISCOVER  ls / launch --list / workspace inspect
CREATE    new --json
PLACE     launch or spawn with --target/--split/--ratio
SHAPE     insert/move/swap exact existing panes when needed
ACT       run, or send-keys for genuinely interactive work
OBSERVE   bounded wait and concurrent bounded watch subprocesses
SURFACE   asked payload + C-a q / C-a Q human guidance
VERIFY    snapshot / agent list / ls
CONFIRM   before destructive signal or kill
```

This is orchestration by explicit terminal identity and observable state. It is
not shared focus, a credential channel, a persistent lease, or a scheduler.
