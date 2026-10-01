---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-09-30
---

# The phux agent CLI

**TL;DR.** Use the headless CLI to create and arrange panes, send input,
read screens, and wait for commands or agent transitions. AgentSession
verbs read and write harness events; pane detection supplies state when no
stream is active. This guide defines safe operation, versioned JSON, and
errors. The generated CLI reference lists flags and defaults.

---

For host setup, start with [coding-agent getting started](./getting-started.md).

## 1. What this is

The CLI's versioned `--json` documents expose screen state, command results,
and events. MCP and the in-tree client library wrap the same functions
without extra privileges. Structured results are local projections, not a
second wire model; see [How phux works](../CONCEPTS.md) and
[ADR-0030](../adr/0030-engine-delegated-wire-and-projection-consumers.md).

Use the generated [CLI reference](../reference/cli.md), `phux --help`, or
`phux <verb> --help` for flags and defaults.

`snapshot`, `wait`, `watch`,
`run`, `send-keys`, `paste`, `ask`, `agent wait`, and
`agent send-keys` neither attach nor resize, and never move an attached
human's focus or viewport. `resize` changes the grid by design but still
never attaches. Layout verbs (`insert-pane`, `move-pane`, `swap-pane`)
change persisted topology, not client-local focus. CLI and MCP cannot hold
an input lease beyond the calling process, so MCP has no `take` / `give`
([MCP adapter](./mcp.md)); `phux take --ttl SECS` asks the server to release
the lease itself after `SECS` (ADR-0033). `phux attach --viewer` watches
without input and `--take` attaches with an input lease (ADR-0127); `phux rec`
and `phux agent log` attach as viewers.

`--socket` wins, then `PHUX_SOCKET`, then the daemon default. `phux ls`
does not auto-start a server.

### This tree, older releases, two agent surfaces

AgentSession is implemented in this checkout, not guaranteed by a minimum
release. The running server must advertise `RESOURCE_KINDS` in
`phux status --json`'s `features`, and the client must expose
`phux agent session`, `phux agent emit`, and `phux agent log`.
`%name` resolves an AgentSession. A server without the capability refuses those
verbs with `unsupported_server` before touching a resource; ordinary pane
operations remain available. Upgrade through your [install source](../INSTALL.md#updating)
when needed rather than assuming that installing a newer adapter upgrades the
server. A live session stream outranks the pane detector.
Harness authors emit into that stream; see the [harness author guide](./harness.md).

The Terminal-scoped surface (`phux agent show`, `explain`, `list`, `set`,
`clear`, `wait`, `send-keys`, `prompt`, `answer`, `start`) projects agent state
onto a pane using `phux.agent/v1`, OSC/title, screen evidence, and any live
session stream. A pane's derived state is not a session log; `%name` does
not match title heuristics.

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

Use `phux run` for a POSIX shell command when you need its exit code. It
brackets the command with printed sentinels and mirrors the child's exit
code (125 when phux gives up), so `phux run … && next` composes like a shell.
Use `send-keys` plus `wait` for a REPL, pager, agent TUI, or other interactive
program where there is no shell sentinel to read.

Because the sentinels are a *typed shell command line*, `run` first
checks that a shell is what reads the pane — the same available-shell
precondition `agent start` applies, from the server-owned
`phux.pane-occupant/v1` record cross-checked with OSC-133 state. Against
`vim`, `less`, or another agent those bytes would be keystrokes, so the
verb refuses with `agent_pane_not_available` (exit 2) and types nothing.
`--force` (MCP: `force: true`) skips the check.

```sh
phux run --json --timeout 120 build "cargo test"
```

To wait for a process to end, spawn it retained and use `phux resource wait`.
The wait cannot miss an exit during startup and can read an earlier exit
while the retained pane keeps its status:

```sh
pane=$(phux spawn --json --retain=600 -- make test | jq -r '"@\(.terminal_id)"')
phux resource wait --json --timeout 900 "$pane"
phux snapshot --json --scrollback 200 "$pane"
```

Exit `0` is an observed exit (`exit.status`, or `exit.signal` for a
death by signal); `1` is `gone` (the resource closed unretained before
the wait, or never existed); `124` is the timeout. Keep the printed
`cursor`: after a disconnect, `--after CURSOR` replays a close the
server still journals. `phux kill --yes "$pane"` purges a retained pane early.

A paste inserts text without submitting it. Bracketed paste (DEC mode 2004)
delivers one block; paste-aware shells and REPLs buffer it until a real Enter.
Follow with `phux send-keys TARGET Enter` to submit. Prefer `paste` for
multiline or indented text: `send-keys` literals type character by character
and can trigger auto-indent. A contiguous literal run immediately before
`Enter` is itself a trusted paste plus the real key.

```sh
phux paste repl "$(cat snippet.py)"
phux send-keys repl Enter
```

To send an agent a prompt and wait for a transition on the same connection:

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

Serialize topology writes: they are last-write-wins. For a fleet workflow
covering discovery, creation, placement, input, asks, and verification, see
[`examples/agents/orchestrate-placed-fleet`](../../examples/agents/orchestrate-placed-fleet).

**Destructive boundary.** Resolve and display the exact target, snapshot
relevant state, explain what will be lost, and obtain affirmative human
confirmation before `kill` or a destructive signal. A watcher ending is
not proof of completion.

## 3. Selectors

One grammar, every targeted verb. Resolved client-side against a
snapshot; the server never parses a selector. The TUI table and
examples live in [pane selectors](./tui.md#selectors). Headless commands reject `=`:
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

Input contracts:

- **`send-keys TARGET KEYS...`** — named keys or literals, tmux-shaped,
  no JSON. Flags must precede `TARGET`. A typo in `agent send-keys` is
  refused before any byte is written; ordinary `send-keys` may type a
  near-miss chord as literal text.
- **`paste TARGET [TEXT]`** — one `INPUT_PASTE` event. Omit `TEXT` to
  read stdin (`git diff | phux paste review`). Trusted by default;
  `--untrusted` opts into the pane's safety gate, which may silently
  drop an unsafe payload (notably multiline). Success includes a
  silently dropped untrusted paste. Never split one logical payload
  across calls. See [the input protocol](../spec/input.md) §5.1.
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

`phux agent start` starts an agent in an existing shell pane without
creating, splitting, moving, or focusing layout.

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
([recording guide](./recording.md)). Capture is viewport-safe in the
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

Producers control record privacy. The [Claude shim](./claude.md#what-the-hook-shim-emits)
omits prompt text and tool input/output; raw hook JSON requires explicit opt-in.

## 7. JSON index

Each `--json` verb stamps its own `schema_version`. The version moves
when a key is **removed, renamed, or retyped**, never when one is added:
consumers ignore unknown keys. Probe for a field's *presence*, not for a
version, when you need to know whether a producer supplies it. Full flag
help: [CLI reference](../reference/cli.md). MCP tool inputs:
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

`unreachable` is **always present**; non-empty means `sessions` and
`terminals` are a lower bound, so branch on `unreachable == []`, never on
diagnostic text. `resources`, `lifecycle` (`running`, `frozen`, `exited`),
`exit` (`{ status, signal, reason, exited_at_ms, retained_until_ms }` for a
retained pane), `keep_empty`, and `empty` are additive: read an absent key as
absent / `running` / `null` / `false`. `kind` is `terminal`, `agent_session`,
or `unknown`. `sessions` lists this host only. `hosts` is the fleet grouped by machine; read it as
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
present and empty (nothing wraps); **absent** (the producer says nothing).
Indices are per-array; a wrapped final `scrollback` index continues into
`lines[0]`. **`truncated` is scoped to the requested window**, not to
rows the emulator evicted. Tolerate an unknown `truncated_reason`. `title`
`null` means no title or an older producer. `--cells` fills a sparse
`cells` array: `{ col, row, semantic?, style }`; `semantic` is `Input` or
`Prompt` (`Output` is absence); `style` is nine SGR booleans plus tagged
`fg` / `bg` (`default`, `palette { index }`, `rgb { r, g, b }`). The right
half of a double-width glyph is skipped.

`--format html|vt` fills `rendered: { format, data }` through the server's
libghostty-vt Formatter (history capped at 10000 rows; over 8 MiB refuses
the read). `data` is UTF-8 for `html` and base64 for `vt`. With a rendering,
`lines` / `scrollback` / `soft_wrap` are omitted and `truncated` is `false`;
`rendered_error` names a render failure the plain projection survived. An
older server silently ignores `--format`, so the CLI/MCP layer turns a
missing `rendered` into a typed failure (exit 2 / tool error). Text output
writes `data` straight to stdout.

### `run` — `RunResult` (no `schema_version`)

```json
{ "command": "cargo test", "exit_code": 0, "output": "...",
  "duration_ms": 8123, "truncated": false }
```

`exit_code` is the child's `$?` from the printed sentinel, not shell
integration. `duration_ms` is wall-clock from the start of the `--timeout` budget (before `TARGET` is resolved), including connection, submission, and poll latency. The capture is scrollback-aware — once the sentinel lands,
`run` re-reads the pane with its retained history — so `truncated` is
true only when the `BEGIN` marker is no longer in that history at all,
not merely when the command outscrolled the viewport. On timeout,
`--json` emits no JSON. MCP `phux_run` reports a tool error, not an
`outcome: "timed_out"` result.

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

`kill` and `signal` take `--idempotency-key HEX32` too (feature
`keyed_signal`): a retry answers the first result, even after the pane is
gone. Through a hub, a keyed retry across a satellite restart is refused with
`INCARNATION_CHANGED`; re-read state and use a new key.

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
lost before they were journaled); `approval_requested.id` and
`approval_decided.{id,outcome}` (`approved`, `denied`, `expired`,
`withdrawn`; ADR-0128: the id names the `phux.approval/v1/<id>` record
`phux approvals` lists). On a journaling server every event
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
0, 1, 124), so branch on `outcome`. `exit` is `null` unless `exited`, and
any fact the client could not learn inside it is `null`. `retained` says
the resource is still listed; `cursor` is `null` without the event journal.
A malformed `--after` is `invalid_cursor` (exit 2); an absence in a partial
fleet view is `partial_view` (exit 3), never `gone`. A resumed wait does not
answer `gone` until replay reaches the journal head, so a close being
replayed reports as the exit it was; `evidence_lost: true` means the replay
hit an evicted range, so a `gone` may hide an exit. A scoped workload
refused the read gets `permission_denied`, exit 2.

`resource show --json` is `{ schema_version: 1, resource, kind, parent,
session, title, cwd, lifecycle, exit, input_holder, viewers, process,
tags, agent, agent_session, unreachable }`. `exit` adds `retained_until_ms`.
`process` is the `GET_TERMINAL_STATE` process facet below, `null` for a
non-Terminal or an older server. `tags` is `null` when the read was
refused. `input_holder` is the lease holder's connection id, or `null`.
`viewers` lists the connection ids attached as `VIEWER` (ADR-0127),
ascending, `[]` when none; the human form prints it only when non-empty.

`resource methods --json` is `{ schema_version: 1, resource, kind,
methods: [{ name, facet, verb, mutating, dangerous, available, reason }] }`.
`dangerous` is the catalog's mark (ADR-0128): sending the method can end a
process, eject a client, or release a held action, so the CLI asks for
`--yes` and MCP for `confirm: true`.
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
[the recording guide](./recording.md).

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
reads as `null`. Normative rules: [resource protocol](../spec/L1.md) §6.3.

**Retained exits.** A Terminal spawned with `retain_secs` stays listed as
`EXITED` with its exit facet after its process ends, still answering screen,
state, history, and attach reads until retention expires, the retained
count bound evicts it, or it is killed; input and signals are refused. Gate
on `RETAIN_ON_EXIT` (normative: [resource protocol](../spec/L1.md),
[ADR-0124](../adr/0124-retain-on-exit.md)).

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
for satellites. `host add` and `host renew` add an `enrollment` object
beside `host`: read its `status` (`enrolled`, `kept`, `failed` with
`error`, `skipped`), not a `null` `client_cert`, to learn whether a
workload client certificate was enrolled; the fields are in
[remote-access.md](../remote-access.md#client-certificates-and-renewal). `pair --json` mints only after the running server
reports a bound remote listener, and otherwise exits 1 with an empty
stdout and nothing minted; the token is a secret emitted once and is not
re-derivable afterwards. `connect_link` is `null` when no address a
device can dial is known. `overlay_addresses` is empty, never absent,
when nothing was detected. `ws_addr` / `quic_addr` are the addresses the
server reports bound, `null` when that transport is not listening.
Rotate/revoke emit operation documents; revoke never includes a token
and needs no server.

Plugin registry enumerates declarative actions, events, panes, and
links from each manifest and does not execute them. Invalid manifests
are hard failures: exit nonzero, stdout empty.

## 8. Exit codes and errors

The canonical table is the [exit-code reference](../reference/exit-codes.md):
`0` success, `1` failure, `2` usage or refusal, `3` partial-fleet
unanswerable, `124` `wait` timeout, `125` `run` timeout.

Per-verb behavior:

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
| `kill` / `signal interrupt\|terminate\|kill` / `detach` / `approve` | `2` without `--yes` when stdin is not a terminal: nothing was sent (ADR-0128). Scripts and agents pass `--yes`. `kill --server` never asks. |
| `approvals` / `approve` / `deny` | `2` for a malformed id, a refused decision (not pending, or no un-held `signal` on the held subject), or `server_too_old`; `1` when the server is unreachable. |

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
`remote_unresolved`. Coordinator startup (`phux server --ensure --json`):
`server_start_timeout`, `server_start_cancelled`, `server_start_failed`.
Resolution: `no_such_target`, `partial_view`.
Sessions: `invalid_session_name` (exit 2: empty, a leading `@`, `#`, or
`%`, or a `:` or `/@` inside, so no selector could name it),
`session_exists`, `session_create_failed` (the server did not confirm a
`new`).
Spawn: `spawn_failed` (the server refused the command or working
directory, or a `--target` placement could not land).
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

A session's one *named shared* projection is its
`phux.tui.layout/v1/<session-id>` L3 envelope: window order, split trees,
pane placement ([metadata protocol](../spec/L3.md) §3.2). The spatial verbs
mutate it; a cross-session move adds one `MOVE_RESOURCE`. Writes are
whole-value last-write-wins, and a spatial edit against a vanished anchor
refuses with a typed code rather than inventing placement (§7). Focus never
rides the envelope: its focus fields are ignored on read, and
`phux.tui.focus/v1` is per-client
([ADR-0049](../adr/0049-client-local-focus-and-advisory-attention.md)).

A script wanting its own arrangement uses its own key prefix
(`app.foo.layout/v1`, [metadata protocol](../spec/L3.md) §3.5) and names it
with `--projection <prefix>.layout/v1/<session-id>` on the spatial and
placement verbs; a cross-session `move-pane` passes it twice or not at all.
Durability is `phux workspace save` / `restore`, which read and replay each
session's split tree from that envelope
([ADR-0129](../adr/0129-projections-are-named-by-key.md)).

## 10. Fallback hierarchy

Prefer the highest rung the target exposes:

1. **Typed command** — `run`, `resize`, `agent emit` / `log`, spatial
   verbs, `tag`, `whoami`: a versioned document or typed refusal.
2. **Semantic stream** — the AgentSession stream and the `EVENT` stream
   behind `watch` / `agent wait`. `EVENT` is best-effort per connection
   (`../spec/L1.md` §7), so keep a poll floor under it.
3. **Text/JSON capture** — `snapshot`, `wait`, `snapshot --format`: a
   rendered projection, fuzzier than a typed field.
4. **Synthetic input** — `send-keys`, `paste`: acknowledgment proves tty
   receipt only, an untrusted paste can be dropped, and it changes program
   state. Last resort.

## 11. Authority: phux is live-state truth

phux is the sole authority for live resource state. Treat events as cues to
re-read that state, not substitutes for it. The AgentSession ring is live and bounded,
not durable evidence
([ADR-0103](../adr/0103-agent-session-resource-and-producer-fed-streams.md)).
The planned durable coordinator is a separate endpoint, not a property of
this stream ([ADR-0097](../adr/0097-durable-coordinator-is-a-separate-bounded-endpoint.md),
[ADR-0095](../adr/0095-the-blackbird-boundary.md)).

## 12. MCP and SDK

- [MCP adapter](./mcp.md) — JSON-RPC stdio adapter over the same verbs.
  `phux mcp --schema` is the tool catalog; `phux mcp --skill` is the
  operating guide. Every verb indexed in §7 has an MCP tool or a listed
  reason it has none, enforced by a parity gate; the mapping is
  [CLI/MCP parity reference](../reference/parity.md).
- [Rust client library](./sdk.md) — `phux-client` is workspace-internal; there is
  no crates.io SDK. Native embedders use `phux-client-ffi`.
- Host adapters: [OpenCode V2](./opencode-v2.md), [Pi](./pi.md),
  [Claude Code](./claude.md). They select subsets; they do not redefine
  this contract.
- Install the reusable skill with `npx skills add no-phux/skills`.
  `phux --skill` prints the version-matched copy compiled into this
  binary. `phux --capabilities --json` reports installed-build discovery,
  not negotiated server state.
