---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-09-15
---

# The phux agent CLI

**TL;DR.** The structured CLI an agent drives without a TTY. Create and
place panes, act with `run`, `send-keys`, or `paste`, observe with
`snapshot`, `wait`, and `watch`, and supervise another agent through
AgentSession verbs or the pane detector. This file is the contract: the
loop, the safety rules `--help` does not teach, the versioned JSON, and
the errors. Flags and inventories live in the generated CLI reference.

---

## 1. What this is

The CLI plus its versioned `--json` documents is the agent contract.
Structured screen state, command results, and semantic events are a local
projection over the shared engine, not a second wire model
([ADR-0030](../adr/0030-engine-delegated-wire-and-projection-consumers.md);
mental model in [`../CONCEPTS.md`](../CONCEPTS.md)). The MCP adapter and
the in-tree client library wrap the same functions; they add no
privilege.

Flags, defaults, and the verb inventory are generated from the binary:
[`../reference/cli.md`](../reference/cli.md), or `phux --help` /
`phux <verb> --help`. This page keeps the facts those texts do not
teach: viewport safety, the read-act-wait loop, selector `%name`, JSON
field meaning, and typed errors.

**Viewport-safe against a live pane.** `snapshot`, `wait`, `watch`,
`run`, `send-keys`, `paste`, `ask`, `agent wait`, and
`agent send-keys` neither attach nor resize. Reads use `GET_SCREEN` /
`GET_METADATA`; input rides `ROUTE_INPUT` to a pane id. None of them
moves an attached human's local focus or viewport. `resize` is the
deliberate exception — changing the grid is its job — and it still never
attaches, so a headless caller cannot drag a pane toward the 80x24 size
a process with no TTY would otherwise report. Layout verbs
(`insert-pane`, `move-pane`, `swap-pane`) change persisted topology, not
client-local focus. CLI and MCP cannot take or give an input lease that
outlives the calling process; MCP therefore exposes no `take` / `give`
tools ([`mcp.md`](./mcp.md)). `phux take --ttl SECS` is a second, orthogonal
deadline (ADR-0033): the server itself now enforces `ttl_ms` and releases
the lease after `SECS` even if the holder never calls `phux give` — a
bound any holder can ask for, CLI or a longer-lived client (the TUI's
take-the-wheel keybinding, a `phux-client`-based agent) alike.
An attach may also declare its intent (ADR-0127): `phux attach --viewer`
watches and can type nothing, and `phux attach --take` attaches and takes the
wheel in one step. `phux rec` and `phux agent log` attach as viewers on a
server that advertises attach roles, so an observer can never type into what
it watches.

`--socket` wins, then `PHUX_SOCKET`, then the daemon default. `phux ls`
does not auto-start a server.

### This tree, older releases, two agent surfaces

This tree serves **AgentSession**. The server advertises
`RESOURCE_KINDS`. `phux agent session open|close`, `phux agent emit`,
and `phux agent log` exist; `%name` resolves an AgentSession. Check
`phux status --json` for `features`. An older brew/curl *release* may
omit the bit; those verbs then refuse with `unsupported_server` before
touching a resource. A live session stream outranks the pane detector.
Harness authors emit into that stream; see [`harness.md`](./harness.md).

The pane **detector** is a different surface: `phux agent show` /
`explain` (and `list` / `set` / `clear` / `wait` / `send-keys` /
`prompt` / `answer` / `start`) project `phux.agent/v1` plus OSC/title
and screen evidence onto a Terminal. Do not treat a detector state as a
session log, or `%name` as a title-heuristic.

A resource has an id (`@N`, or `host/@N` behind a hub), a kind, an
optional parent, a lifecycle, an output stream, and an input channel its
kind defines. **Terminal** is a PTY plus a libghostty engine. **AgentSession**
is a coding-agent run (`provider`, optional `native_id`, derived
`state`) whose stream is one JSON record per event, appended by the
harness through `phux agent emit`. An agent session always has a
Terminal parent, set at spawn and never changed; closing the parent
closes the child, never the other way round. A **pane** is the TUI word
for a Terminal in a layout slot; an agent session is never a pane.

`@N` may name either kind. A Terminal-facet verb (`snapshot`,
`send-keys`, `run`, `resize`, …) refuses an agent-session id with
`wrong_resource_kind`. `phux ls --json` tells the kinds apart.

## 2. The loop

Read → act → wait → read. Every wait carries a finite timeout. Type
something that is not already on the screen, then wait for that text:

```sh
phux send-keys . "printf '%s\n' phux-ready | tr a-z A-Z" Enter
phux wait --until "PHUX-READY" --timeout 10 .
phux snapshot --json --scrollback 50 .
```

`phux run` is the one-shot for a POSIX shell command whose `$?` you
want. It brackets the command with printed sentinels and **mirrors the
child's exit code** (125 when phux itself gives up), so
`phux run … && next` composes like a shell. `send-keys` plus `wait` is
the loop for an interactive or long-lived program — a REPL, a pager, an
agent TUI — where there is no sentinel to harvest.

```sh
phux run --json --timeout 120 build "cargo test"
```

For a process whose **end** you need (a build, a test run, a one-shot
job), spawn it retained and wait on its exit. `phux resource wait` is
race-free: an exit that happens while the wait starts is never missed,
and a process that already exited is still read, because the retained
pane keeps its status:

```sh
pane=$(phux spawn --json --retain=600 -- make test | jq -r '"@\(.terminal_id)"')
phux resource wait --json --timeout 900 "$pane"
phux snapshot --json --scrollback 200 "$pane"
```

Exit `0` is an observed exit (`exit.status`, or `exit.signal` for a
death by signal); `1` is `gone` (the resource closed unretained before
the wait, or never existed); `124` is the timeout. Keep the printed
`cursor`: after a disconnect, `--after CURSOR` replays a close the
server still journals. `phux kill "$pane"` purges a retained pane early.

A paste **inserts**; it does not **submit**. Bracketed paste (DEC mode
2004) delivers one block; paste-aware shells and REPLs buffer it until a
real Enter. Follow with `phux send-keys TARGET Enter` to run what you
pasted. Prefer `paste` for multiline or indented text: ordinary
`send-keys` literals type character by character and will trip
auto-indent. A contiguous literal run immediately before `Enter` is
itself a trusted paste plus the real key.

```sh
phux paste repl "$(cat snippet.py)"
phux send-keys repl Enter
```

Supervising another agent is the same loop with a different wait. Prefer
the fused prompt when you need the write and the edge on one connection:

```sh
phux agent prompt --expect-agent reviewer --wait \
  --until idle --until blocked --timeout 900 --json @7 "review the diff"
phux snapshot --json --tail 200 --unwrap @7 > transcript.json
```

Exit `0` means a transition into a requested state was **observed**.
`124` means none was — not "still working". `1` is a departure (record
gone, or `state` withdrawn to `unknown`). Neither is completion. The
level read is `phux agent show`; `agent wait` / `agent prompt --wait`
are edge reads and time out on a pane already resting in the target
state.

The fleet extension is discover → create → place → shape → act →
observe → surface asks → verify. Topology writes are last-write-wins:
serialize them. A worked script:
[`examples/agents/orchestrate-placed-fleet`](../../examples/agents/orchestrate-placed-fleet).

**Destructive boundary.** Resolve and display the exact target, snapshot
relevant state, explain what will be lost, and obtain affirmative human
confirmation before `kill` or a destructive signal. A watcher ending is
not proof of completion.

## 3. Selectors

One grammar, every targeted verb. Resolved client-side against a
snapshot; the server never parses a selector. The TUI table and
examples live in [`tui.md`](./tui.md#selectors). Headless commands reject `=`:
they have no attached-client focus history.

| Selector | Meaning |
|---|---|
| `.` | focused session / pane |
| `name` | session |
| `name:N` / `name:tag` | window |
| `name:N.M` | pane |
| `@N` | local resource id (either kind) |
| `host/@N` | satellite resource through a hub |
| `#tag` | every Terminal tagged `tag` (where the verb accepts a set) |
| `%name` | the AgentSession named `name`, or its parent Terminal |

`%name` is singular: exactly one match or a refusal that lists the
candidates (exit 2). It resolves against live AgentSession resources
first — the name is the `phux.agent/v1` `name` on the parent Terminal —
and falls back to a Terminal with that named record and no session
child. Handed to an agent-session verb (`emit`, `log`, `session close`)
it yields the session; handed to a Terminal-facet verb it yields the
parent pane, so `phux agent prompt %reviewer` and
`phux agent log %reviewer` name the same agent from two sides. Two live
sessions sharing a name refuse. `name:N.M` and the window forms resolve
Terminals only.

A selector that names several panes narrows to one: the focused pane if
it is among the matches, else the first in snapshot order. Spatial and
placement targets must each resolve to exactly one local pane.
`snapshot`, `wait`, `watch`, and `agent wait` may omit a target;
`send-keys`, `paste`, `run`, `ask`, `resize`, and the acknowledged agent
writes require one.

## 4. Input: send-keys, paste, run

Help text owns the flags. The contract:

- **`send-keys TARGET KEYS...`** — named keys or literals, tmux-shaped,
  no JSON. Flags must precede `TARGET`. A typo in `agent send-keys` is
  refused before any byte is written; ordinary `send-keys` may type a
  near-miss chord as literal text.
- **`paste TARGET [TEXT]`** — one `INPUT_PASTE` event. Omit `TEXT` to
  read stdin (`git diff | phux paste review`). Trusted by default;
  `--untrusted` opts into the pane's safety gate, which may silently
  drop an unsafe payload (notably multiline). Success includes a
  silently dropped untrusted paste. Never split one logical payload
  across calls. See [`../spec/input.md`](../spec/input.md) §5.1.
- **`run TARGET CMD...`** — POSIX shell, sentinels, mirrors `$?`. Flags
  must precede `TARGET`. `--timeout` (default 600s; `0` means none) is
  one absolute budget started before `TARGET` is resolved: connect,
  handshake, resolution, input submission, every screen read, and the
  sleeps between reads all draw on it, and the last sleep is clipped to
  what remains. A server that accepts the connection but never answers
  still ends with exit 125 on time. Input is never started once the
  budget has run out; once started it gets a short grace to finish, and
  the timeout diagnostic says whether submission was not sent, partial,
  or complete. On timeout there is **no JSON**; the signal is exit 125
  plus stderr.

Acknowledged agent writes (`agent send-keys`, `agent prompt`,
`agent answer`) prove kernel tty-queue receipt, not consumption.
`delivery_unknown` is terminal: inspect the pane and do not resend.
The server has one acknowledged input lane; serialize concurrent
acknowledged writes.

`phux agent start` starts an agent inside an **existing** shell pane.
It never creates, splits, moves, or focuses layout.

## 5. Observe: snapshot, wait, watch, ask

- **`snapshot`** — side-effect-free `GET_SCREEN`. `--json` emits
  `ScreenState`. `--tail` never returns a partial grid: the viewport is
  a floor. `--unwrap` joins soft-wrapped rows; it cannot combine with
  `--cells`.
- **`wait`** — poll that same read until a condition. Matching is
  against logical lines (wraps joined). `--tail` on `wait` is **not** a
  viewport floor: `--tail 3` means three lines. `--output-only` drops
  OSC-133 `Input` lines; with no marks it filters nothing and says so on
  stderr. `--json` emits the final `ScreenState` as read — projections
  scope the match, not the document. `--until` and `--regex` are
  mutually exclusive; an invalid regex is exit 2 before any poll.
  `--timeout` is one absolute budget started before `TARGET` is
  resolved (connect, handshake, resolution, the `--output-only` probe,
  every screen read, and poll sleeps); the last sleep is clipped to what
  remains. The first screen read always gets at least 2 seconds from the
  start, so `--timeout 0` means "check once, now". A wedged server still
  ends the wait with exit 124; with `--json`, expiry before the first
  completed read emits an empty default `ScreenState`.
- **`watch`** — push events, neither attach nor resize. `--json` is
  NDJSON, **no `schema_version`**, versioned by the binary and the
  `event` name vocabulary (a follower may join mid-stream). `--until`
  turns the stream into a gate; `--timeout` exits 124 without appending
  a summary. `agent_state` is the detector half: one `(scope, key)`
  subscription on the resolved pane, not a fleet-wide stream.
  `command_started` / `command_finished` come from OSC 133 `C` / `D` in
  the raw PTY bytes; a shell with no integration never emits them, and
  `idle` is only as quiet as the prompt. On a server with the event
  journal (`event_journal` in `phux status --json`) the last stderr line
  is the cursor this run reached; `--after CURSOR` resumes from it, and
  events the journal still holds are replayed before live ones.
- **`resource wait`** — block until one resource's process ends, then
  report how (exit 0), `gone` (exit 1), or the timeout (exit 124). A
  direct `@N` target is used as given, so a pane that already closed can
  still be named. Subscribe-then-read on one connection makes it
  race-free; it is the completion gate for a process, as `agent wait` is
  for an agent.
- **`resource show`** — one resource's record: kind, parent, lifecycle,
  the exit of a retained process, the typed process facts (pid,
  foreground group, cwd, prompt state), the input-lease holder and the
  connections watching it as viewers, tags, and agent record. `resource methods` lists what the resource answers
  on this server; listing grants nothing.
- **`ask`** — advisory human attention. It does not move focus. The
  reference TUI presents it as `C-a q` / `C-a Q`.

`phux rec` and `phux play` are the recording surface
([`recording.md`](./recording.md)). Capture is viewport-safe in the
same sense as `snapshot` and `watch`. `snapshot --rendered` is the
exception that attaches a headless client to composite the frame.

## 6. AgentSession verbs vs detector verbs

### Detector (Terminal-scoped)

`phux agent list|show|explain`, `set`, `clear`, `wait`, `send-keys`,
`prompt`, `answer`, `start`. A declared `phux.agent/v1` record outranks
heuristics. Omitting `--state` on `set` writes `"unknown"`, which is
identity only: the detector fills `state`. Any other `--state` stands
derivation down on that pane until `clear`, reap, or withdrawal (the
declaring process died; the server sets `state` to `"unknown"` and keeps
`name` / `kind` / `session`).

Where state comes from, in precedence: **Stream > Hook > Process >
Screen**. A live AgentSession child's event stream outranks
`phux agent report-state`, which outranks foreground-process identity,
which outranks screen rules. Idle stays the detector's to derive. `agent
show` names the winning rung in `sources[0].kind` (`stream` when the
session stream decided it) and reports the session under
`agent_session`.

`agent wait` is edge-triggered. `--any` waits for the first matching
transition from any local agent in the fleet; it cannot be combined with a
`TARGET`. The client subscribes to server-wide resource lifecycle events
before enumerating panes, installs one `phux.agent/v1` subscription per local
Terminal, follows resource creation and closure, and periodically re-enumerates
as a loss-recovery floor. An agent already resting in a requested state only
establishes its baseline and does not satisfy the fleet wait. Satellite panes
remain excluded because L3 metadata is hub-local. A satellite `TARGET` is
refused (`satellite_target`, exit 2); run the wait on that satellite's own
server. `watch` still carries the pane's agent *events* across the hub.

`--expect-agent` matches `name`. A detector-written `name` is a per-kind
constant (`claude` on every Claude pane), not a per-pane label. Set
`--name reviewer` yourself when you need identity.

`phux agent explain --file` is offline: it evaluates detection manifests
against a capture and contacts no server.

### AgentSession (resource-scoped)

`phux agent session open TARGET --provider P` spawns the child and makes
the caller its **producer**. The server does not deduplicate: a second
open on the same pane is a second session. `session close` closes the
session and leaves the parent pane. `emit` appends one record of the
closed v1 `type` set: `session_start`, `prompt`, `tool_start`,
`tool_end`, `notification`, `ask`, `stop`, `session_end`, `state`,
`provider_raw`. The server stamps `seq` and `ts_ms`. Refusals write
nothing: `not_producer`, `record_invalid`, `overflow`,
`wrong_resource_kind`. `log` reads the retained ring; `--follow` is a
stream with no `--timeout` (run it under a child-process deadline).

Privacy belongs to the producer. The shipped Claude shim's `prompt`
carries `{"chars": N}` and never the text; tool records carry
`tool_name` and never input or output; `provider_raw` is emitted only
when `PHUX_AGENT_EMIT_RAW=1`.

## 7. JSON index

Each `--json` verb stamps its own `schema_version`. The version moves
when a key is **removed, renamed, or retyped**, never when one is added:
consumers ignore unknown keys. Probe for a field's *presence*, not for a
version, when you need to know whether a producer supplies it. Full flag
help: [`../reference/cli.md`](../reference/cli.md). MCP tool inputs:
`phux mcp --schema`.

On failure, **stdout stays empty** and stderr carries one JSON error
object (exceptions noted). `--json` is the long flag; there is no `-j`.

### `ls` (`schema_version` 3)

```json
{
  "schema_version": 3,
  "sessions": [
    { "name": "work", "windows": 1, "attached": true, "attached_clients": 1,
      "keep_empty": false, "empty": false }
  ],
  "terminals": ["@3"],
  "resources": [
    { "id": "@3", "kind": "terminal", "parent": null,
      "lifecycle": "running", "exit": null },
    { "id": "@9", "kind": "agent_session", "parent": "@3",
      "lifecycle": "running", "exit": null }
  ],
  "hosts": [],
  "hosts_complete": true,
  "unreachable": []
}
```

`unreachable` is **always present**. Non-empty means `sessions` and
`terminals` are a lower bound, not an inventory; branch on
`unreachable == []`, never on diagnostic substrings. An absent key is a
pre-v3 binary. `terminals` is the Terminal-kind inventory. `resources`
is additive: omit it on an older server; `kind` is `terminal` or
`agent_session` (unknown kinds render as `unknown`). `lifecycle`
(`running`, `frozen`, or `exited`) and `exit` are additive: `exited`
with an `exit` object `{ status, signal, reason, exited_at_ms,
retained_until_ms }` is a pane spawned with `--retain` whose process
ended; read an absent key as `running` / `null`. `keep_empty` /
`empty` are additive; read an absent key as `false`. `sessions` lists
this host only. `hosts` is the fleet grouped by machine; read it as
complete only when `hosts_complete` is `true` (the server advertises
`HOST_SESSIONS`). `host` is `null` for this machine (`local: true`).
`id` in `hosts[].sessions` is the session's id **on its own host**,
never comparable across hosts. `panes` counts Terminal-kind resources.
`active_terminal` is the remembered focused pane as a canonical
selector — `host/@N` through a hub. An unreachable satellite stays
listed with `reachable: false` and no sessions.

### `snapshot` / `wait` — `ScreenState` (`schema_version` 3)

```json
{
  "schema_version": 3,
  "pane": 3,
  "cols": 120,
  "rows": 40,
  "cursor": { "x": 0, "y": 12, "visible": true },
  "lines": ["$ cargo test"],
  "scrollback": [],
  "cells": null,
  "soft_wrap": { "lines": [], "scrollback": [] },
  "truncated": false,
  "truncated_reason": null,
  "title": "phux",
  "rendered": null,
  "rendered_error": null
}
```

`scrollback` is tri-state on the wire: flag absent → viewport only;
`--scrollback` / `0` → all retained history; `N` → most-recent N rows.
**`soft_wrap` is three-way:** present and non-empty (these rows wrap);
present and empty (wrapping was reported, nothing wraps); **absent**
(the producer says nothing — today, a server predating the field).
Indices are per-array; a wrapped final `scrollback` index continues into
`lines[0]`. **`truncated` is scoped to the requested window**, not to
rows the emulator evicted from its history ring. `truncated_reason` is a
string; tolerate an unknown value. `title` `None` means no title *or* a
producer that predates the field. `--cells` fills a sparse `cells`
array: `{ col, row, semantic?, style }`. `semantic` is `Input` or
`Prompt`; `Output` is collapsed to absence. `style` is nine SGR
booleans plus `fg` / `bg`, each a tagged `CellColor` (`default`,
`palette` `{ index }`, or `rgb` `{ r, g, b }`) so "terminal default"
is distinct from "explicitly black". The right half of a double-width
glyph is skipped. `--format html|vt` fills `rendered`:
`{ format: "html" | "vt", data }`, rendered through the server's own
libghostty-vt Formatter, its history window capped at 10000 rows and its
byte size checked before allocation (over 8 MiB refuses the whole read with
a `resource_exhausted`-class error, naming the budget) — `data` is UTF-8
text for `"html"`, standard base64 for `"vt"` (VT output is not guaranteed
valid UTF-8). `--unwrap` with `--format` asks the Formatter itself to join
soft-wrapped rows, rather than the client-side `unwrap` this surface uses
otherwise. When a rendering is requested, `lines`/`scrollback`/`soft_wrap`
are omitted from the reply (the capture already carries that text) and
`truncated` reads `false`. `rendered` is `null` when no rendering was
requested, or when one was requested but the CLI/MCP call itself failed
(see below); `rendered_error` names a same-server render failure that the
plain projection survived. **No server negotiation gates `--format`**: a
server predating it silently answers as if it were absent, so the CLI/MCP
layer detects a missing `rendered` after a non-`None` request and reports a
typed failure (`phux snapshot`: exit 2; MCP: a tool error) instead of quietly
returning success with no capture. Text output writes `data` straight to
stdout instead of the boxed view (VT decoded to raw bytes); `--json` keeps
the whole document.

### `run` — `RunResult` (no `schema_version`)

```json
{ "command": "cargo test", "exit_code": 0, "output": "...",
  "duration_ms": 8123, "truncated": false }
```

`exit_code` is the child's `$?` from the printed sentinel, not shell
integration. `duration_ms` is wall-clock from the start of the `--timeout` budget (before `TARGET` is resolved), including connection, submission, and poll latency. `truncated` is true
when the `BEGIN` marker scrolled out of the viewport. **On timeout,
`--json` emits no JSON.** Do not expect `outcome: "timed_out"` here;
that shape is MCP `phux_run`.

### `new`

```json
{ "schema_version": 1, "session": "work", "terminal_id": 2 }
```

Create-only: `--json` requires `-s NAME` (exit 2 if omitted) and fails
if the name exists. Before reporting success it stores the session's initial
single-pane layout, so spatial verbs can place or move that pane without a
sacrificial interactive attach. A new live Terminal starts at the 80x24
headless geometry; automatic window-size policies return to that geometry
after the last view detaches, while `manual` holds an explicit `phux resize`.
`--empty --json` has `"terminal_id": null` plus
`empty` / `keep_empty`. Terminal-facet verbs against an empty session
fail immediately with `no_such_target`. `--idempotency-key HEX32`
(32 hex digits, drawn once per request and reused on every retry) makes
the create safe to retry: a repeat answers the first create's result
instead of failing on the name. It needs `spawn_idempotency` in
`phux status --json` `features`; otherwise it is refused before any
write with `unsupported_server`, exit 2.

### `spawn` / `launch` / spatial

```json
{ "schema_version": 1, "terminal_id": 7, "satellite": null, "replayed": false }
```

`satellite` is the registry name when routed with `--satellite`; then
`terminal_id` is the id *on that satellite*. `spawn --retain[=SECS]`
keeps the pane inspectable after its process exits (bare `--retain`:
the server's default), until the time passes or `phux kill`; write
`--retain=SECS` when a command follows. `spawn --idempotency-key HEX32`
makes a retry answer the first pane with `replayed: true` instead of
spawning another; the same key with a different request is
`idempotency_conflict`, exit 2. Each flag needs its feature
(`retain_on_exit`, `spawn_idempotency`); without it the spawn is refused
before sending with `unsupported_server`, exit 2, never silently
ignored. Launch adds `integration`,
`plugin`, and the resolved `argv`. `--list` / `--print` are separate
documents; placement does not add a second success shape.

`kill` and `signal` take `--idempotency-key HEX32` too, for a supervisor
that must not act twice: a retry under the same key answers the first
result instead of killing or signalling again, and a keyed `kill @N` is
sent as written, so a retry after the pane is already gone still gets the
first answer. Both need `keyed_signal` in `features`; without it they are
refused before sending with `unsupported_server`, exit 2. Through a hub, a
keyed retry whose satellite restarted in between is refused with
`INCARNATION_CHANGED` and nothing reaches the satellite: read state and
decide afresh under a new key.

Spatial edits emit `schema_version` 1 with `operation` and `session_id`.
`direction` is the CLI divider (`vertical` = side-by-side,
`horizontal` = stacked). A cross-session move adds `source_session_id`
and `cross_session: true`. Stable refusal codes include
`invalid_selector`, `selector_miss`, `selector_not_single`,
`satellite_target`, `cross_session`, `same_pane`, `invalid_ratio`,
`layout_missing`, `pane_not_in_layout`, `pane_already_in_layout`,
`layout_rejected`. Cross-session moves may also report `server_too_old`,
`move_refused`, `post_move_state_failed`, `destination_changed`,
`destination_layout_failed`, `source_layout_failed` (exit 1 once
ownership work has begun; preflight stays exit 2).

### detector — `AgentStateJson`

```json
{
  "schema_version": 1,
  "agents": [
    {
      "terminal": "@3",
      "session": "work",
      "window": "window-0",
      "agent": { "id": "claude", "label": "Claude", "kind": "claude" },
      "state": "working",
      "confidence": 0.95,
      "attention": "normal",
      "title": "claude",
      "cwd": "/repo",
      "sources": [
        { "kind": "stream", "signal": "tool_start", "confidence": 1.0,
          "observed": "tool_start" }
      ],
      "explanation": "live agent-session stream",
      "agent_session": {
        "resource": "@9", "provider": "claude", "native_id": "sess-01H..."
      }
    }
  ]
}
```

`agent_session` is additive (`null` when the pane has no live child).
The key is `agent_session`, not `session` — `session` is already the
phux session name. `sources[].kind` includes `stream`, `agent_record`,
`title_ask`, `screen`, `semantic_cells`, `identity`, `plugin_report`.
`state`: `unknown|idle|working|blocked|done`. `explain` JSON always
includes the evidence trail; the human view is what expands.

`agent wait --json` (timeout still on **stdout**, exit 124):

```json
{
  "schema_version": 1,
  "terminal": "@7",
  "satisfied": true,
  "edge": { "from": "working", "to": "idle", "via": "push" },
  "baseline": "working",
  "state": "idle",
  "agent": { "name": "reviewer", "kind": "claude", "session": null },
  "observations": { "edges": 1, "pushes": 2, "polls": 3 },
  "detection": null
}
```

`edge` is `null` exactly when `satisfied` is `false`. `via` is `"push"`
or `"poll"` (the re-read floor recovering a dropped notification).
`baseline` is recorded and **never evaluated**. `detection` is one
`agents[]` entry for this pane, or `null` when the post-wait read fails.
With `--any`, a successful document has the same shape and names the Terminal
that won the race; `observations` additionally carries `agents`. On timeout,
`terminal`, `edge`, `baseline`, `state`, and `agent` are `null`.

`agent send-keys --json` is emitted only on a fully delivered batch:
`verified`, `delivery` (`ok`), `operation_id`, `attempts`, `keys`.
`agent prompt --json` records both halves; on wait timeout it still goes
to stdout with `delivery: "ok"` and exit 124. `agent answer --json`
names the live ask and `source` (`choice` or `text`). `agent start
--json` includes `ready` and a `readiness` object; `--no-wait` leaves
`ready: false` and `readiness: null`. A readiness timeout is an error
document on stderr, exit 124 — the command was already typed.

Offline `explain --file --json` is a different document: top-level key
`explain`, not `agents`. Branch on `detector_state` (what the detector
would publish), not on `state` (what a rule asserted; absent when none
did). When they differ, `fallback_reason` says which case applied.
`regions` lists every region the grammar offers, including empty ones.
`evaluated_rules` includes misses; `evidence` is the predicate tree with
per-node `matched`.

### AgentSession — `open` / `emit` / `log`

```json
{ "schema_version": 1, "resource": "@9", "parent": "@3",
  "provider": "claude", "native_id": "sess-01H..." }
```

`native_id` is `null` when omitted. `emit --json` echoes the stamped
header only (`resource`, `seq`, `ts_ms`, `type`). `log --json` without
`--follow` wraps `records` in that same envelope; `--tail N` trims
`records` and says nothing else. Under `--follow`, stdout is NDJSON —
one record per line, no envelope, no `schema_version`, same rule as
`watch`. An unknown `type` is printed, not dropped: the server refused
unknown types at append, so an unknown one here means a newer server.

### `watch --json` (NDJSON, no envelope)

Each line is `{ "event": <name>, "terminal"?: "@id", ... }`. Payload
fields: `title_changed.title`; `pane_closed.exit_status`;
`asked.{id,question,suggestions,elapsed_seconds}` (`elapsed_seconds`
nullable); `command_finished.exit_code` (nullable only when the `D` mark
omits it or the shell has no OSC-133); `agent_state.{name,kind,session,
state,attention,from}` — `from` is the state last seen *in this watch
run*, absent on the first record; `attention` is derived from `state`; a
deleted record emits `state: null` rather than dropping the line.
`cwd_changed.cwd`; `terminal_control.{lifecycle,action,exit_status,
input_holder,actor_client}` (`action: exited` is a retained pane's
process ending); `journal_gap.{first_missing,last_missing}` (this watch
missed that range: re-read level state); `source_gap.dropped` (events
lost before they were journaled). On a journaling server every event
line also carries `seq`, `ts_ms`, and `actor` (`{ client,
credential_id, client_name }`) when present; a `journal_gap` line has
none. After the stream ends, the last stderr line is the cursor: under
`--json` one object `{ "cursor": "SERVER_ID:SEQ", "cursor_void":
false }`; `cursor_void: true` means the `--after` cursor came from
another server run and the stream started live. `--until unknown` still
matches every event outside the gate names frozen at 1.0 (`agent_state`,
`asked`, `bell`, `command_finished`, `command_started`, `dirty`, `idle`,
`pane_closed`, `pane_spawned`, `title_changed`, `unknown`), including
`cwd_changed`, `terminal_control`, `journal_gap`, and `source_gap`, which
printed as `unknown` before they had names; an existing `--until unknown`
gate keeps working, and the new names gate on exactly one kind.

### `resource show` / `resource wait` / `resource methods`

```json
{ "schema_version": 1, "resource": "@7", "outcome": "exited",
  "exit": { "status": 42, "signal": null, "reason": "exited",
            "exited_at_ms": 1757800000123 },
  "retained": true, "waited_ms": 1834, "cursor": "9f1c...:118",
  "evidence_lost": false }
```

That is `resource wait --json`. `outcome` is `exited`, `gone`, or
`timed_out`; the document is printed on **stdout for all three** (exit
0, 1, 124), so branch on `outcome`. `exit` is `null` unless `exited`;
inside it any fact the client could not learn is `null` (a close seen
only as an event carries no `reason`). `retained` says the resource is
still listed. `cursor` is `null` on a server without the event journal.
A malformed `--after` is `invalid_cursor`, exit 2; an absence in a
partial fleet view is `partial_view`, exit 3, never `gone`. A resumed
wait does not answer `gone` until the replay has reached the server's
journal head at the snapshot cut, so a close still being replayed is
reported as the exit it was. That head is the connection's own (the
newest event its subscription admits), so other panes' events never
keep a resumed wait on a gone pane waiting. `evidence_lost: true` means
the replay reported a range the journal had already evicted: a close in
that range was never seen, so a `gone` may hide an exit (resume sooner,
or retain the pane). A wait that saw no event returns that head
as its `cursor`, so a resume replays nothing already accounted for, and
an error exit names the cursor it reached in its remedy. A scoped
workload refused the subscription or the state read gets
`permission_denied`, exit 2.

`resource show --json` is `{ schema_version: 1, resource, kind, parent,
session, title, cwd, lifecycle, exit, input_holder, viewers, process,
tags, agent, agent_session, unreachable }`. `exit` adds `retained_until_ms`.
`process` is the `GET_TERMINAL_STATE` process facet below, `null` for a
non-Terminal or an older server. `tags` is `null` when the read was
refused. `input_holder` is the lease holder's connection id, or `null`.
`viewers` lists the connection ids attached as `VIEWER` (ADR-0127),
ascending, `[]` when none; the human form prints it only when non-empty.

`resource methods --json` is `{ schema_version: 1, resource, kind,
methods: [{ name, facet, verb, mutating, available, reason }] }`.
`facet` is `substrate` or the kind that owns the method; `verb` is the
closed verb label (`OBSERVE`, `BIND+OBSERVE`, `none`); `reason` is
`null` when available, else `feature_unadvertised`, `wrong_kind`,
`transport`, or `unimplemented`. Discovery grants nothing:
authorization happens when the method is sent.

### `resize`

```json
{ "schema_version": 1, "terminal_id": 7,
  "requested": { "cols": 120, "rows": 40 },
  "applied": { "cols": 120, "rows": 40 }, "held": true }
```

`applied` is read back from the server. **The object is printed on the
geometry-mismatch path too** (exit 1, `held: false`); transport failures
still leave stdout empty. Without `--json` the applied size prints as
`120x40`.

### `tag` / `whoami` / `ask` / `rec` / `play`

`tag` returns `{ schema_version: 1, terminals: [{ terminal, tags }] }`.
`terminal` is the reusable selector (`@N` / `host/@N`). `tags` after
`add`/`rm` is read back from the server, never echoed. An untagged
Terminal is `[]`, not an absent key.

`whoami` is the server's `phux.whoami/v1` record passed through:
`principal`, `credential_id` (null on a route with no credential; never
a token), `auth_route` (open vocabulary: `uds`, `ssh-stdio`,
`bearer-quic`, …), `peer_uid`, `serving_user`, `host`,
`server_version`, `ssh_client` (set only on `ssh-stdio`).
`serving_user.name` is null when the uid has no password-database
entry. `ssh_client` is a report from `SSH_CONNECTION`, not an
authenticated fact, and grants nothing beyond `uds`. An older server is
`server_too_old`, exit 1. MCP `phux_whoami` takes `socket` only, not
`--remote`.

`ask --json` echoes `{ schema_version, event: "asked", terminal, id,
question, suggestions, elapsed_seconds }` after the server accepts.

`rec --json`: `{ schema_version, path, format, bytes, duration_ms,
frames, cols, rows, truncated }`. `format` is `cast`, `gif`, or `apng`.
`duration_ms` is the recording's timeline after the idle clamp, not wall
time. `frames` is encoded animation frames; for `cast` it is the event
count. `truncated` is true when encoding stopped at `--max-bytes`: the
file is still a complete playable container. A failed *export* is exit
1 but keeps the captured `.cast`. Full surface in
[`recording.md`](./recording.md).

`play --json`: `{ schema_version, terminal_id, path, cols, rows, events,
speed, idle_limit, duration_ms, passes }`. `path` is absolute (the pane
resolves it from the daemon's cwd). `cols`/`rows` are the recording's
grid. `duration_ms` is playback length at the requested speed after the
idle clamp. `idle_limit` is `null` when none was applied. `passes` is
`null` when it repeats until killed. The verb returns as soon as the
pane exists; poll `snapshot` for the final frame unless `--close`. A
failure creates no pane.

### `GET_TERMINAL_STATE` — `process` facet (`schema_version` 1, wire only)

`phux resource show --json` embeds the `process` object; SDKs that send
the `GET_TERMINAL_STATE` command read the whole document. The `process`
object is
`{ child, foreground, cwd, prompt, exit }`, and the Rust type is
`phux_core::process::TerminalProcessState`:

```json
{ "child": { "pid": 4242, "start_ms": 1757800000123 },
  "foreground": { "pgid": 4250, "start_ms": 1757800009876, "name": "vim" },
  "cwd": "/repo", "prompt": { "state": "running", "last_exit_code": 0 },
  "exit": null }
```

Every key is present; `null` means the server could not find out, never
"none". Pair `pid` with `start_ms` before trusting a pid across calls.
`prompt.state` is `unknown|at_prompt|running`, from OSC-133 marks.
`exit.signal` reports a death by signal that `RESOURCE_CLOSED.exit_status`
reads as `null`. Normative rules: [`../spec/L1.md`](../spec/L1.md) §6.3.

**Retained exits.** A Terminal spawned with `SPAWN_RESOURCE.retain_secs`
(ADR-0124; `0` asks for the server default, `defaults.retain-on-exit-secs`)
does not close when its process exits. The server emits `terminal_control`
with `action: exited` and the exit status, and keeps the resource: `GET_STATE`
lists it with `lifecycle: EXITED` and an exit facet (`exit_status`, `signal`,
`reason`, `exited_at_ms`, `retained_until_ms`), and `GET_SCREEN`,
`GET_TERMINAL_STATE` (the same exit as `process.exit`), history, and
`ATTACH_RESOURCE` keep answering. Input answers `INPUT_NOT_WRITTEN`;
`SIGNAL_TERMINAL` answers `INVALID_COMMAND`. So a waiter that arrives after
the exit reads the status instead of `TERMINAL_NOT_FOUND`: subscribe to the
Terminal's events, then read `GET_STATE`. The resource closes with the
ordinary `RESOURCE_CLOSED` when retention expires, when the server's retained
count bound (`defaults.retain-on-exit-max`) evicts it, oldest first, or when
you `KILL_RESOURCE` it (`reason: KILLED`, idempotent). A retained Terminal holds
its grid and history until then (up to `defaults.history-bytes`, so roughly
512 MiB at the default bound of 256 with the 2 MiB default) but no
pseudoterminal or descriptor. Check `RETAIN_ON_EXIT`
in `HELLO_OK` first; a server without it closes the Terminal at exit. From the
CLI: `phux spawn --retain[=SECS]` sets the field (refused with
`unsupported_server` when the bit is absent), and `phux resource wait` is that
waiter.

### Other `--json` verbs

`config agents` is `schema_version` 2: top-level `state` / `attention`
are *effective* values (runtime record first, declared manifest as
fallback). `live` is whether a server answered; `source` is `"runtime"`
or `"manifest"`; `runtime` is `null` for manifest rows. Identity match
is record `kind` slug, else lowercased `name`, equals the agent id.
Several panes declaring the same agent report the most attention-worthy
binding. Attention: declared value first, else derived from state
(blocked→high, working→normal, done/unknown→low, idle→none). An active
ask on a record that declares *no* state elevates it to `blocked`; a
declared record state outranks the ask.

`config run`: `outcome` is `"completed"` or `"timed_out"`; `exit_code`
is `null` when the OS provides none or phux kills the child on timeout.

Workspace inspect is repo-local git porcelain. Detached worktrees have
`branch: null` and `detached: true`. Archive schema 2 copies
`agent_session` as inert provenance — restore re-resolves the current
integration, requires the same `plugin_id`, and never replays archived
argv. `command` is nullable. Restore starts fresh PTYs; existing session
names are skipped (`restored` / `skipped_existing`). Schema-1 archives
remain readable.

`host ls`: `enabled` is `null` for `role: "remote"`; `session` is `null`
for satellites. `pair --json` never contacts a server; the token is a
secret emitted once and is not re-derivable afterwards. `connect_link`
is `null` when no address source exists. `overlay_addresses` is empty,
never absent, when nothing was detected. `ws_addr` / `quic_addr` are
configured binds, not resolved dials. Rotate/revoke emit operation
documents; revoke never includes a token.

Plugin registry enumerates declarative actions, events, panes, and
links from each manifest and does not execute them. Invalid manifests
are hard failures: exit nonzero, stdout empty.

## 8. Exit codes and errors

The canonical table is [`../reference/exit-codes.md`](../reference/exit-codes.md):
`0` success, `1` failure, `2` usage or refusal, `3` partial-fleet
unanswerable, `124` `wait` timeout, `125` `run` timeout.

Mirroring that `--help` does not collect in one place:

| Verb | Notes |
|---|---|
| `run` | child's `$?` clamped to `0..=255`; `125` when phux gave up. Uses 125, not 124, because the child may legitimately exit 124. |
| `wait` / `watch` / `agent wait` / `agent prompt --wait` | `0` met; `124` timeout; `2` usage. |
| `paste` | `0` includes a silently dropped untrusted paste. |
| `agent wait` | `1` is departure, never completion. Already-in-state times out. |
| `agent send-keys` / `prompt` / `answer` | `0` kernel-queue receipt; `2` pre-write refusal; `1` transport or `delivery_unknown`. |
| `agent session open\|close` / `emit` / `log` | `2` for `wrong_resource_kind`, `unsupported_server`, `not_producer`, `record_invalid`, `overflow`. A closed session is `no_such_target` (exit 1), not a refusal. `log` has no 124. |
| `resize` | `0` only when the server holds the requested size. |
| `resource wait` | `0` exited (now or earlier, while retained); `1` `gone`; `124` timeout; `2` usage (`invalid_cursor`) or a scope that cannot observe the resource (`permission_denied`, answered at once rather than at the deadline); `3` absent from a partial fleet. The document is on stdout for `0`, `1`, and `124`. |
| `resource show` / `resource methods` | `1` `no_such_target`; `3` when the miss is against an incomplete fleet. |
| `spawn` / `new` with `--retain` / `--idempotency-key` | `2` for `unsupported_server` (feature not advertised; nothing sent), `invalid_idempotency_key`, and `idempotency_conflict` (spawn only). A keyed `new` whose key already belongs to another create request registers nothing and exits `1`: a create result has no refusal form. |
| `kill` / `tag` / `agent show\|set\|clear` | `3` when the miss is against an incomplete fleet. |

**Exit `3`.** A federation hub that cannot reach a satellite still
answers `GET_STATE` with those panes missing. A miss then has two
causes: the target does not exist (`1`) or the server could not look
where it lives (`3`). Retry is right for `3` and wrong for `1`. Some
verbs cannot spend the status (`run` mirrors the child; `wait` owns
124) and keep `1` while saying it on stderr. Session-name verbs never
return `3`: the session name space is complete even when the fleet is
not. Enumerators (`ls`, `agent list`) warn and exit `0`; `ls --json`
reports it in `unreachable`.

JSON error object on stderr:

```json
{
  "schema_version": 1,
  "error": { "code": "no_server", "message": "no server running at /run/phux.sock" },
  "remedy": "start one with `phux` or `phux server`",
  "exit_code": 1
}
```

Branch on `error.code`, never on `message`. `remedy` is always present.
Transport: `no_server`, `server_disconnected`, `transport`,
`remote_unresolved`. Resolution: `no_such_target`, `partial_view`.
Local I/O: `io` (a bug-report bundle could not be written under the
state directory), `json_serialize`.
Agent lifecycle: `no_agent_record`, `satellite_target`,
`agent_departed`, `agent_mismatch`, `invalid_key_spec`. Acknowledged
input: `input_busy` (retry safe), `input_not_written` (proven not
delivered; retry safe), `delivery_unknown` (never resend),
`input_too_large`, `input_lease_held`, `canonical_limit_exceeded`,
`unsafe_paste`, `invalid_input_batch`, `permission_denied`. Ask:
`no_active_ask`, `ask_unidentified`, `ask_stale`,
`answer_choice_out_of_range`, `answer_not_suggested`. Start:
`invalid_agent_name`, `unsupported_agent_kind`,
`agent_detection_unavailable`, `agent_name_conflict`, `target_not_shell`,
`invalid_launch_argv`, `ambiguous_integration`, `agent_start_timeout`,
`agent_kind_mismatch`. Resource: `wrong_resource_kind`, `not_producer`,
`record_invalid`, `overflow`, `unsupported_server`. Offline explain:
`capture_unreadable`, `capture_invalid`, `unknown_agent_kind`. Watch:
`unknown_event_name`.

## 9. Projection scoping

A session has exactly one *named shared* projection: its
`phux.tui.layout/v1/<session-id>` L3 envelope (schema v3), holding window
order, split trees, and pane placement
([`../spec/L3.md`](../spec/L3.md) §3.2). `insert-pane`, `move-pane`, and
`swap-pane` mutate that envelope; a cross-session move additionally issues
one `MOVE_RESOURCE` (L1). Layout is a consumer projection of the shared
engine, not wire state, generalized from Terminal to every resource kind
by [ADR-0102](../adr/0102-resources-the-server-serves-kinds.md)
(superseding [ADR-0030](../adr/0030-engine-delegated-wire-and-projection-consumers.md)).

**Focus never rides the shared envelope.** The envelope's
`focused_window_index` and per-window `focused_terminal` fields stay in
the schema for compatibility, but a reader ignores them on
reconciliation and repairs local focus deterministically instead
([ADR-0049](../adr/0049-client-local-focus-and-advisory-attention.md)).
Separately, `phux.tui.focus/v1` is per-client metadata — namespaced by
client UUID — and is never synchronized across clients
([`../spec/L3.md`](../spec/L3.md) §3.3). No script moves another client's
viewport by writing layout.

**When the anchor pane vanished concurrently,** phux does not invent
placement: a spatial edit against a missing or already-placed pane
refuses with a typed code (`pane_not_in_layout`, `pane_already_in_layout`,
`layout_rejected`, `destination_changed`; the full table is above, in §7's
`spawn` / `launch` / spatial JSON index). A write that does land is
whole-value last-write-wins, so concurrent writers converge on one value
rather than merging (`../spec/L3.md` §3.2).

**A script wanting its own arrangement uses its own key prefix** —
`app.foo.layout/v1` rather than the TUI's schema — per
[`../spec/L3.md`](../spec/L3.md) §3.5. Sharing the TUI's layout schema is
opt-in, not the default.

**`--projection KEY` names that choice on the CLI.** `insert-pane`,
`move-pane`, `swap-pane`, and the placement flags on `spawn` / `launch`
accept `--projection <prefix>.layout/v1/<session-id>` naming any envelope
of the §3.2 shape for the session addressed; omitting it keeps the shared
default. A cross-session `move-pane` touches two distinct envelopes — pass
`--projection` twice (source and destination, either order) or not at all,
never exactly once. There is still no projection resource: naming a key is
the whole mechanism, and durability is `phux workspace save` / `restore`
replaying the archived split tree, not server-held state
([ADR-0129](../adr/0129-projections-are-named-by-key.md)).

`phux workspace save` reads each session's real split tree from that same
envelope (`--projection KEY` there names the key *prefix* to read every
session's own copy from, default the shared one); `GET_STATE` never carries
one, so a session with nothing stored falls back to a bare pane list.
`phux workspace restore` replays every archived pane, not only a session's
seed process — including a native agent session's own resume, where it
still resolves to the plugin that owns it — and rolls back just the one
session on a partial failure rather than the whole archive.

## 10. Fallback hierarchy

Rank affordances by how much of the answer is typed fact versus inferred
from raw bytes, and prefer the higher rung:

1. **Typed command.** `run`, `resize`, `agent emit`/`log`, the
   spatial verbs, `tag`, `whoami` — a versioned JSON document or a typed
   refusal code, checked by the server before anything is inferred.
2. **Semantic stream.** The AgentSession stream (`emit`/`log`, one JSON
   record per producer-stamped event) and the `EVENT` / `AgentEvent` push
   stream behind `watch` and `agent wait`. Lower latency than polling, but
   today's `EVENT` stream is "an additive accelerator ... not a normative
   structured contract" (`../spec/L1.md` §7): delivery is best-effort per
   connection, and a slow subscriber's mailbox drops silently rather than
   gapping. A consumer that depends on it still needs the poll floor
   underneath, the way `agent wait` already runs one.
3. **Text/JSON capture.** `GET_SCREEN` / `snapshot`, and the `wait` poll
   built on it. This is a rendered projection of the shared engine's grid
   — lines, optional per-cell `semantic` tags (`Input` / `Prompt`), a
   viewport or scrollback window — not a typed fact about the process
   behind it. Matching text is fuzzier than checking a typed field, and a
   truncated or soft-wrapped read can misrepresent a line the process
   never emitted that way. `snapshot --format html|vt` (`../spec/L1.md`
   §6.1) rides the same rung: the server renders the capture through its
   own libghostty-vt Formatter (never reimplemented, per CONTRIBUTING)
   into HTML with inline styles or re-playable VT escape sequences, for a
   consumer that wants styling or an exact byte-for-byte replay rather
   than the plain-text/JSON projection — still a rendered projection, not
   a typed fact.
4. **Synthetic input.** `send-keys`, `paste`, and their wire form
   (`ROUTE_INPUT` fire-and-forget, `APPLY_INPUT` acknowledged). This is
   the fallback of last resort: acknowledgment proves kernel tty-queue
   receipt, not that the target program consumed or acted on the input
   (§4); an untrusted paste can be silently dropped by the pane's safety
   gate; and unlike every rung above, this one changes program state by
   emulating a human's keystrokes rather than reading or calling a typed
   surface — the least observable rung and the one carrying the most risk.

No rung below typed commands is authoritative on its own: read the
highest rung the target kind actually exposes.

## 11. Authority: phux is live-state truth

phux is the sole authority for live resource state: the current
lifecycle, screen content, and process facts of every Terminal and
AgentSession it runs. Its own push stream says as much of itself —
`EVENT` is a convenience accelerator, not a normative structured
contract, and a consumer that ignores it still converges by polling
(`../spec/L1.md` §7). The AgentSession stream is retained as a bounded
ring: "live and bounded, not durable evidence"
([ADR-0103](../adr/0103-agent-session-resource-and-producer-fed-streams.md)).
A durable *work* record — objectives, runs, artifacts — belongs to a
separate, independently versioned coordinator endpoint that references
phux resources without carrying their bytes
([ADR-0097](../adr/0097-durable-coordinator-is-a-separate-bounded-endpoint.md)).

Today phux and Blackbird do not connect at all: Blackbird holds no phux
workload key, and phux writes nothing to Blackbird beyond one optional
field inside `phux.agent/v1` that lets the two ledgers be joined after
the fact ([ADR-0095](../adr/0095-the-blackbird-boundary.md)). The rule
that follows applies to any external durable-work journal, present or
future: it may hold and journal work intent, but any signal it receives —
a phux event included — is a cue to re-read phux's current state, never a
substitute for it. Treat an event as wake-then-read: it tells an agent to
look, not what it will find.

## 12. MCP and SDK

- [`mcp.md`](./mcp.md) — JSON-RPC stdio adapter over the same verbs.
  `phux mcp --schema` is the tool catalog; `phux mcp --skill` is the
  operating guide. Every verb indexed in §7 has an MCP tool or a listed
  reason it has none, enforced by a parity gate; the mapping is
  [`../reference/parity.md`](../reference/parity.md).
- [`sdk.md`](./sdk.md) — `phux-client` is workspace-internal; there is
  no crates.io SDK. Native embedders use `phux-client-ffi`.
- Host adapters: [`opencode.md`](./opencode.md), [`pi.md`](./pi.md),
  [`claude.md`](./claude.md). They select subsets; they do not redefine
  this contract.
- `phux --skill` prints the guide compiled into this binary.
  `phux --capabilities --json` reports installed-build discovery, not
  negotiated server state.
