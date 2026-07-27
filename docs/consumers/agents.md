---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-07-15
---

# The phux agent CLI

**TL;DR.** The structured CLI surface an AI agent drives without a TTY:
create with `new`, place configured agents or explicit argv with `launch` /
`spawn`, reshape exact existing panes with `insert-pane` / `move-pane` /
`swap-pane`, act through `run`, `send-keys`, or `paste`, observe through bounded `wait` /
`watch`, and raise advisory human attention with `ask`. Plugin, workspace, and
satellite verbs provide the surrounding configuration and inventory surfaces.
This file is the agent contract. Per ADR-0030, the structured agent state —
cells, command results, semantic events — is a local projection over the shared
engine, and the CLI plus its versioned JSON schemas are what an agent depends
on, not a structured wire tier. It documents each verb, its JSON shape, the
read-act-wait loop, and the exit codes each verb mirrors.

---

## 0. The thesis: structured agent state is a projection

phux does not own terminal semantics; libghostty does, and both ends of the
wire run that engine
([ADR-0013](../../ADR/0013-libghostty-bytes-on-wire.md)). It follows that any
structured view of a terminal — a cell grid, an OSC-133 command-boundary
stream, a command's captured output — is computed by a consumer from the
engine it already has, not transmitted as a second model on the wire
([ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md)).

So the agent contract is **not** a structured wire protocol. It is this CLI and
the versioned JSON schemas its `--json` verbs emit. The wire carries opaque
terminal bytes plus lifecycle and metadata; the structured shapes below are a
local projection an agent reads through the CLI
([ADR-0022](../../ADR/0022-tool-for-agents.md): agents are a projection, the
CLI plus JSON schema is the contract). An agent that wants to own its own
projection — run the engine and read its grid directly — should copy
[phux-web](./web.md), the reference carry-your-own-engine consumer
(ADR-0030 §4).

The live wire does expose agent affordances: `GET_SCREEN`, `ROUTE_INPUT`,
`GET_TERMINAL_STATE`, `SUBSCRIBE_TERMINAL_EVENTS`, and an `AgentEvent` push
frame, documented in [`../spec/L1.md`](../spec/L1.md). Read those as
engine-convenience snapshots over the shared engine — a convenience for
consumers that have not adopted the carry-your-own-engine pattern — not a
normative structured contract and not a license to add new structured wire
surface (ADR-0030 §2).

## 1. What this is, what this isn't

This document is the agent-facing CLI surface, parallel to the
[TUI's product surface](./tui.md) and the [MCP adapter](./mcp.md). The TUI
projects the source-of-truth `Terminal` to VT bytes (it renders, like tmux);
agents project it to structured data — cells, OSC-133 marks, command results.

The agent surfaces nest:

- **This CLI** is the canonical, stable agent contract: the verbs and their
  `--json` shapes are what an agent depends on.
- [`mcp.md`](./mcp.md) is a thin adapter that wraps the same `phux-client`
  functions name-for-name over JSON-RPC stdio.
- [`sdk.md`](./sdk.md) documents `phux-client` itself — the library crate the
  CLI and MCP adapter are both built from. It exists today; it is L1-shaped and
  follows the same projection pattern.

All three are unprivileged consumers
([ADR-0017](../../ADR/0017-tui-not-protocol-privileged.md)); none holds a
protocol-level privilege. The wire underneath stays additive and versioned,
normative under [`../spec/`](../spec/). Installing `phux-mcp` does not make its
tools visible to a host; see [Registering with a host](./mcp.md#registering-with-a-host)
for the Claude Code command and generic stdio configuration.

The selector grammar is owned by [`tui.md`](./tui.md) §3; this file links there
rather than restating the table (the doc system's one-fact-one-home rule). The
decision rationale lives in
[ADR-0022](../../ADR/0022-tool-for-agents.md); client-side selector resolution
in [ADR-0021](../../ADR/0021-control-plane-commands.md).

**Viewport-safe against a live pane.** `snapshot`, `run`, `send-keys`,
`paste`, and `wait` neither attach nor resize the target pane: reads issue
`GET_SCREEN`, and input rides `ROUTE_INPUT` to a pane id. `snapshot`/`wait`
are side-effect-free; `run`/`send-keys`/`paste` deliberately mutate the live
PTY. None changes an attached human's local focus or viewport. `resize` is
the deliberate exception — changing the grid is its whole job — but it too
never attaches, so it cannot drag a pane toward the 80x24 size a caller with
no TTY would otherwise report.

## 2. The structured CLI surface (verb catalog)

phux is one binary; the verbs below are its agent-facing subcommands.
[`tui.md`](./tui.md) §1 has the full CLI table; this section zooms into the
agent verbs and their JSON. Exit codes are collected in §5.2.

- **`phux ls [--json] [--socket P]`** — list sessions. Does not auto-start a
  server (like `tmux ls`): with none running it reports as much and exits
  non-zero. `--json` emits `SessionListJson` (§4.1).
- **`phux snapshot [--json] [--scrollback[=N]] [--cells] [--socket P]
  [TARGET]`** — side-effect-free pane read via `GET_SCREEN`. `TARGET` is
  optional (defaults to the focused session). `--json` emits `ScreenState`
  (§4.2); without it, a boxed text view.
- **`phux send-keys [--socket P] TARGET KEYS...`** — route named keys or
  literal strings to one resolved pane by id (`ROUTE_INPUT`). `TARGET` is
  required. No JSON. `KEYS` are tmux-shaped: named keys (`Enter`, `Tab`,
  `Escape`, `Up`, `C-c`, `M-x`) or literal strings. Literals normally type
  character by character. A contiguous literal run immediately before
  `Enter`/`Return` becomes one trusted paste followed by the real Enter key;
  this honors live bracketed-paste mode so agent TUIs cannot absorb the
  submit key into a fast text burst.
- **`phux paste [--untrusted] [--socket P] TARGET [TEXT]`** — deliver a
  payload to one resolved pane as a single paste event (`ROUTE_INPUT`).
  `TARGET` is required; `TEXT` is the payload, read from stdin when omitted
  (`git diff | phux paste review`). No JSON, like `send-keys`; exit codes
  mirror `send-keys` (§5.2). The server picks the delivery form from the
  pane's live terminal state: when the pane's program has bracketed paste
  (DEC mode 2004) switched on, the payload arrives wrapped in `ESC[200~` /
  `ESC[201~` markers as one block; otherwise the raw bytes are delivered as
  if typed. **A paste INSERTS; it does not SUBMIT.** Paste-aware shells and
  REPLs (bash on readline 8.1+, python 3.13+'s PyREPL) buffer the bracketed
  block and wait for a real Enter — follow with
  `phux send-keys TARGET Enter` to run what you pasted. Prefer `paste` for
  anything multiline or indented when you want insertion without submission:
  ordinary `send-keys` literals type character by character, while a dedicated
  paste arrives intact. The submission shorthand
  `phux send-keys TARGET "text" Enter` is also bracketed-paste-aware.
  Pastes are **trusted by default** — the agent vouches for content it
  composed, the same ungated input authority `send-keys` has. `--untrusted`
  opts into the server's safety gate: the payload is classified, and the
  pane's untrusted-paste policy (reject, by default) may silently drop an
  unsafe payload — notably anything multiline — so reserve the flag for
  content you did not compose and cannot vouch for.
- **`phux run [--timeout SECS] [--json] [--socket P] TARGET CMD...`** — run a
  command in a pane and capture its exit code, output, and duration via printed
  sentinels (assumes a POSIX shell: sh/bash/zsh). `TARGET` is required.
  `--json` emits `RunResult` (§4.3). The exit code mirrors the child (§5.2).
  Flags must precede `TARGET`, or clap's `trailing_var_arg` swallows them into the
  command line.
- **`phux wait [--until TEXT] [--idle MS] [--timeout SECS] [--json]
  [--socket P] [TARGET]`** — poll the side-effect-free screen read until a condition
  holds. `--until` takes precedence over `--idle`; with neither, it settles on
  idle. `--json` emits the final `ScreenState`. Exit 0 when the condition is
  met, 124 on timeout. Two gotchas: flags must precede `TARGET`; and `--until`
  matches any visible row, including the shell's echo of the command you just
  typed — match on text that appears only in command output, never the command
  itself.
- **`phux watch [--json] [--socket P] [TARGET]`** — stream a pane's live events
  (the push half of the agent surface; see [`../spec/L1.md`](../spec/L1.md)).
  Subscribes to the server's event stream scoped to the resolved pane and
  prints one event per line until EOF (server gone) or Ctrl-C; the
  subscription neither attaches nor resizes the pane. With `--json`, each line
  is a JSON object `{ "event": <name>, "terminal"?: "@id", ... }` and stdout
  stays pure JSON (diagnostics on stderr); otherwise a compact tab-separated
  human line. Event names: `title_changed` (carries `title`), `bell`, `dirty`,
  `idle`, `pane_spawned`, `pane_closed` (carries `exit_status`), `asked`
  (carries `id`, `question`, `suggestions`, and nullable `elapsed_seconds`),
  plus the deferred `command_started` / `command_finished` (carries a nullable
  `exit_code` — see the gap note below). `watch` cuts `wait`'s poll-floor
  latency: a `watch` consumer wakes the instant an event fires rather than on
  the next poll tick. It is additive — `wait` still works without it, and a
  dropped event (full mailbox) falls back to polling.
  **Deferred:** `command_started` / `command_finished` are wire-allocated but
  not emitted by the current server (the OSC-133 command boundary is not
  cleanly observable without disturbing the per-consumer state-sync
  synthesizer); `command_finished.exit_code` is likewise always null until that
  shell-integration plumbing lands. The mechanism and the
  lifecycle/title/bell/dirty/idle events ship today.
- **`phux rec -o PATH [--format FMT] [--from FILE] [--duration SECS]
  [--fps N] [--idle-limit SECS] [--max-bytes N] [--cast-version N] [--json]
  [--socket P] [TARGET]`** — record a pane and export it as an asciinema cast,
  an animated GIF, or an APNG. `-o`/`--out` is the only required argument; its
  extension picks the format (`.cast`, `.gif`, `.png`/`.apng`; no extension
  means GIF) unless `--format` overrides it. Capture subscribes with `ATTACH_TERMINAL` and is
  viewport-safe in the same sense as `snapshot` and `watch`: it neither
  attaches the session nor resizes the pane. `--from` re-renders an existing
  cast offline and never contacts the server. `--json` emits the result object
  (§4.14). Ctrl-C stops the capture and still writes the artifact, so an
  open-ended `phux rec -o demo.gif` under a subprocess deadline is the scripted
  shape. Full surface in [`recording.md`](./recording.md).
- **`phux ask TARGET [--id ID] [--suggest TEXT...] [--elapsed-seconds SECS]
  [--json] [--socket P] QUESTION`** — report that an agent in a pane is blocked
  on a human-answerable question. This is the opt-in hook ingress from
  ADR-0036: configured plugin actions or first-party integrations call it
  instead of writing a `phux-ask` title sentinel themselves. It resolves
  `TARGET` client-side, does not attach or resize, and asks the server to emit
  the normal `asked` event on the existing watch stream. `--json` echoes the
  reported `{ event, terminal, id, question, suggestions, elapsed_seconds }`
  object after the server accepts the payload. Empty questions, empty
  suggestions, excessive suggestion counts, and unknown panes fail without
  emitting an event. The reference TUI presents that event as advisory
  attention: `C-a q` cycles asking panes and `C-a Q` returns to the saved local
  origin. A headless agent reports the ask and prints that guidance; it does not
  move focus.
- **`phux agent <list|show|explain> [TARGET] [--json] [--socket P]`** —
  project public agent state. A pane carrying a declared `phux.agent/v1`
  record (ADR-0040; see `agent set` below) reports straight from it with
  `agent_record` provenance and no heuristics; otherwise state is inferred
  from already-phux-shaped evidence: session/pane metadata, OSC/title hints,
  side-effect-free `snapshot --cells`, and enabled plugin `[[agents]]`
  declarations. `list` covers every pane; `show` returns the selected pane;
  `explain` keeps the same state but expands the evidence trail in the human
  view. `--json` emits `AgentStateJson` (§4.7). States are `unknown`, `idle`,
  `working`, `blocked`, or `done`; each state carries confidence and ordered
  provenance so consumers can show why phux believes it.
- **`phux agent set [TARGET] --name NAME [--kind K] [--state S]
  [--attention A] [--session L] [--socket P]`** — declare the target pane's
  agent identity by writing the whole `phux.agent/v1` L3 record
  ([`docs/spec/L3.md`](../spec/L3.md) §3.7, ADR-0040; last writer wins). An
  agent integration calls it (or issues the equivalent `SET_METADATA`) when
  it starts, changes state, or hands off, instead of encoding lifecycle into
  its OSC title. The declared record outranks title/screen heuristics in
  every consumer, and the reference TUI labels the pane's window/sidebar tab
  from it. States: `unknown|idle|working|blocked|done`; attention:
  `none|low|normal|high` (defaults derive from state). Prints the confirmed
  record as `@N<TAB>json`.
- **`phux agent clear [TARGET] [--socket P]`** — delete the declared record
  (`DELETE_METADATA`); consumers fall back to the OSC-title and screen
  heuristics. Prints `@N<TAB>-` on confirmation.
- **`phux agent install-claude [--shell zsh|bash|fish] [--real PATH]`** —
  make plain interactive `claude` invocations enter phux automatically. The
  installer leaves the real Claude binary untouched, writes a phux-owned shim
  under `$XDG_DATA_HOME/phux/shims`, and adds one marked PATH block to the
  detected shell rc. Outside phux, the shim creates and attaches a new session
  in the caller's working directory; inside a pane it runs Claude in place.
  Claude lifecycle hooks publish `working`, `blocked`, and `done` records, and
  blocked notifications also emit `phux ask`, so phone and TUI fleet views see
  attention without screen inference. Noninteractive/admin invocations such as
  `claude -p`, `claude mcp`, and `claude --version` bypass phux.
- **`phux agent uninstall-claude`** — remove only the phux-owned shim, hook
  settings, manifest, and marked shell-rc block. User shell configuration and
  the real Claude installation are otherwise untouched.
- **`phux resize [--json] [--socket P] TARGET COLSxROWS`** — set one resolved
  pane's grid, with no TTY. `TARGET` is required; `COLSxROWS` is two whole
  numbers of cells, each at least 1 (`120x40`). This is the only way to size a
  pane without a terminal: every other path derives geometry from an attached
  client's *viewport*, and a caller with no TTY reports 80x24. Nothing
  attaches and nothing subscribes, so the call cannot itself shrink the pane
  it is sizing.

  The new size applies immediately, even with someone attached, and it is not
  permanent against an attached view: under the default
  `defaults.window-size = "smallest"` policy the next attach, detach, or
  window resize recomputes the pane's geometry from the attached views and
  supersedes it. `window-size = "manual"` is the setting under which an
  explicit resize holds ([`tui.md`](./tui.md) §4.2). You do not have to guess
  which happened — the verb reads the server's real geometry back before
  exiting and exits `1` when it is not the requested one. Shape in §4.15.
- **`phux new [-s NAME] [-c CWD] [-- COMMAND...] [--json] [--socket P]`** —
  create a new session. Without `--json` it creates and attaches: an explicit
  `-s NAME` that already exists is an error (like tmux's duplicate-session
  refusal); an omitted name starts from `defaults.session-name-template` and
  gains a numeric suffix when needed; a server is auto-spawned if none is
  running. With `--json` it
  creates the session without attaching (no attach, no resize), then prints the
  seed pane id as JSON and exits. `--json` requires an explicit `-s NAME` and
  errors if that name is already in use (create-only, never create-or-attach).
  Shape in §4.4.
- **`phux launch INTEGRATION [--list|--print] [--target TARGET
  [--split horizontal|vertical] [--ratio R]] [-c CWD] [--json] [--socket P]
  [-- ARGS...]`** — resolve an enabled plugin integration and spawn it through
  its identity wrapper. `--list` inventories integrations; `--print`/`--dry-run`
  resolves argv without a server; `--target` places the launched pane beside an
  exact local pane. Successful `--json` launch shape is in §4.13.
- **`phux spawn [--satellite NAME] [--target TARGET [--split horizontal|vertical]
  [--ratio R]] [-c CWD] [-- COMMAND...] [--json] [--socket P]`** — spawn a
  terminal without attaching (`SPAWN_TERMINAL`). With `--target`, the new pane
  is owned by the target's exact local window and inserted beside it; `vertical`
  means side-by-side and `horizontal` means stacked; `R` is
  finite and strictly between 0 and 1. Without placement flags, the pane joins
  the server's most recently active session (legacy behavior). The new terminal
  id prints on success. `--satellite NAME` routes the spawn
  through a federation hub (`phux server --hub`) to the named registry
  satellite and prints the satellite-tagged id, which every
  satellite-capable verb can address through the hub. Does not auto-start
  a server. Typed failures (unknown/unrouted satellite, unreachable link)
  exit nonzero with the diagnostic on stderr. Shape in §4.11.
- **`phux insert-pane TARGET NEW_PANE [--horizontal|--vertical] [--ratio R]
  [--json] [--socket P]`** — insert an already-created local pane beside an
  existing layout leaf. This never spawns: create `NEW_PANE` separately first.
  Both selectors must each match exactly one pane in the same session.
  `--vertical` means a vertical divider (side-by-side panes); `--horizontal`
  means a horizontal divider (stacked panes) and is the default. Shape in §4.12.
- **`phux move-pane SOURCE TARGET [--horizontal|--vertical] [--ratio R]
  [--json] [--socket P]`** — collapse `SOURCE` out of its old position and
  insert it beside `TARGET` in the same session. Shape in §4.12.
- **`phux swap-pane FIRST SECOND [--json] [--socket P]`** — exchange two leaf
  positions without changing split geometry. Shape in §4.12. All three spatial
  verbs reject multi-match, satellite, and cross-session selectors and do not
  change an attached client's local focus.
- **`phux plugin <list|link|unlink|enable|disable|validate> [--json]`** —
  manage declarative plugin manifest entries in the local config registry.
  This never contacts a running server and never executes plugin commands.
  `--json` emits the plugin registry document (§4.5); failure paths leave
  stdout empty and report diagnostics on stderr.
- **`phux config agents [--json] [--socket PATH]`** — project configured
  plugin `[[agents]]` declarations into a flat agent-state list, merged with
  live per-pane `phux.agent/v1` records and asked state when a server answers
  on the socket (phux-r82.10). No reachable server degrades to the declared
  manifest values. `--json` emits `ConfiguredAgentsJson` (§4.6).
- **`phux config run PLUGIN ACTION [--timeout SECS] [--cwd PATH] [--json]`** —
  execute one action declared by an enabled configured plugin manifest. The
  command runs as argv from the plugin root; there is no implicit shell
  expansion. `--json` emits `PluginActionOutput` (§4.8). Exit code mirrors the
  action's process status; timeout exits `125`.
- **`phux workspace inspect [PATH] [--json]`** — inspect the local git
  repository containing `PATH` and every checked-out worktree reported by git.
  This never contacts a running server and never creates, deletes, or checks out
  worktrees. Agents use the JSON shape (§4.9) to choose a checkout before
  creating a session (`phux new -c <worktree>`) or mapping existing sessions and
  panes back to repo paths.
- **`phux workspace save [--socket P] [--output PATH]`** — capture the running
  phux workspace as a typed JSON archive. With no `--output`, the archive is
  printed to stdout. This contacts the server but does not attach or resize.
- **`phux workspace restore ARCHIVE [--socket P]`** — recreate sessions missing
  from a saved archive. Restore starts new processes; it does not claim to
  resurrect the original PTYs.
- **`phux satellite <list|add|remove> [--json]`** — manage the hub-side
  federation satellite registry. This never contacts a running server and never
  opens a satellite transport; it only edits `[[satellites]]` in local config.
  `--json` emits the satellite registry document (§4.10); failure paths leave
  stdout empty and report diagnostics on stderr.

`insert-pane` is intentionally not named `split`: it edits topology around a
pane that already exists and performs no implicit spawn. Spawn-and-place remains
a separate operation. `detach` is still an interactive TUI action. The shipped
verbs are listed in [`tui.md`](./tui.md) §1.

**Destructive boundary.** An agent must resolve and display the exact target,
snapshot relevant state, explain what will be lost, and obtain affirmative human
confirmation before `kill` or a destructive signal. The MCP signal adapter also
requires `confirm: true` for interrupt/terminate/kill. A watcher ending is not
proof of completion; verify inventory or terminal state under a finite bound.

**How `new` decomposes on the wire.** Session create is no longer an L1
session verb. Per
[ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md) §5,
the session lifecycle verbs were removed from L1 and decompose into substrate
primitives plus L3 metadata: `new` is `SPAWN_TERMINAL` plus an L3 metadata
write on the `phux.session.create/v1` key (the assigned identity is read back
via `phux.session.created/v1`), and rename is an L3 metadata SET on the
`phux.session.name/v1` key. Grouping conventions are owned by
[`../spec/L3.md`](../spec/L3.md). The user-facing UX of `new` is unchanged; the
divergence is on the wire, where the migration to this decomposition is tracked
against ADR-0030 (full `GroupId` removal is bead phux-0bmc).

**Socket precedence (once, for every verb).** The `--socket` argument wins,
then the `PHUX_SOCKET` environment variable, then the daemon default:
`$XDG_RUNTIME_DIR/phux/phux.sock`, falling back to `/tmp/phux-$UID/phux.sock`.

## 3. Targeting: the selector grammar

One grammar, every targeted command — `kill`, `snapshot`, `wait`, `watch`,
`send-keys`, `paste`, `run`, `ask`, `resize`, launch/spawn placement, and the
three spatial verbs all share `TARGET`.
It is resolved client-side against a server snapshot ([ADR-0021](../../ADR/0021-control-plane-commands.md)); the server
never parses a selector.

The full grammar table and CLI examples live in [`tui.md`](./tui.md) §3. In one
line, the forms are: `.` (current), `name` (session), `name:N` / `name:tag`
(window), `name:N.M` (pane), and `@N` (opaque id). `=` is explicitly
unsupported for headless commands because they have no attached-client MRU.

A selector that names several panes (a whole session or window) narrows to a
single pane: the focused pane when it is among the matches, else the first in
snapshot order (the `pick_target_pane` tiebreak the MCP tools share).
Optionality differs per verb: `snapshot`, `wait`, and `watch` may omit a target;
`send-keys`, `paste`, `run`, `ask`, `resize`, and every spatial verb require it. `launch`/`spawn`
use an optional target only for explicit local placement. Spatial and placement
targets are stricter than the selected-pane tiebreak: each must resolve to one
exact local pane.

## 4. JSON contracts (the per-verb machine shapes)

Each `--json` verb emits a versioned, plain-data struct from `phux-core` or
`phux-client`. These structs are the stable agent contract
([ADR-0022](../../ADR/0022-tool-for-agents.md)); they are a local projection
over the shared engine, and the wire underneath stays additive and versioned.
Each struct carries its own `schema_version`, tracked independently.

### 4.1 `SessionListJson` — `phux ls --json`

Defined in `crates/phux-core/src/session_list.rs` (`LS_SCHEMA_VERSION = 3`).
Version 2 added the aggregate `terminals` inventory; version 3 adds
`unreachable`. Shape, name-sorted:

```json
{
  "schema_version": 3,
  "sessions": [
    { "name": "work", "windows": 3, "attached": true }
  ],
  "terminals": ["@3", "devbox/@7"],
  "unreachable": []
}
```

`windows` is the window count; `attached` is a bool — whether any client is
attached. `terminals` is the complete addressable inventory in snapshot order,
using the canonical direct selector syntax. Satellite entries intentionally do
not imply a hub-local session/window join.

**`unreachable` is how you know the listing is complete.** A federation hub
that cannot reach a satellite still answers — it merges what it has and never
fails the list (§ [`tui.md`](./tui.md) federation) — so without this field a
partial inventory is byte-identical to a whole one. Each entry is one hub
diagnostic naming a satellite that contributed nothing
(`"satellite build-box is unreachable: link is down"`). The **presence** of
entries is the contract; the prose is a diagnostic, so branch on
`unreachable == []`, never on substrings. The key is emitted **even when
empty**, deliberately: an absent key is what a pre-v3 `phux` produces, and a
consumer cannot tell that apart from a degraded answer. Treat `sessions` and
`terminals` as a lower bound whenever it is non-empty.

**Cross-surface gotcha:** the MCP
`phux_ls` tool ([`mcp.md`](./mcp.md)
§3.1) surfaces the raw wire fields `window_count` / `attached_client_count`;
the CLI's `--json` projects them to `windows` / `attached`. The two surfaces do
not share identical keys — do not carry a parser across them.

### 4.2 `ScreenState` — `phux snapshot --json` (and `phux wait --json`)

Defined in `crates/phux-core/src/screen.rs` (`SCHEMA_VERSION = 3`). The same
struct the server returns from `GET_SCREEN`, not an agents-specific shape.
Fields:

| Field | Type | Meaning |
|---|---|---|
| `schema_version` | u32 | Contract version (currently `3`); the pin/branch signal. |
| `pane` | u32 | Wire-local id of the captured pane. |
| `cols`, `rows` | u16 | Grid dimensions. |
| `cursor` | `Option<{x,y,visible}>` | Viewport-relative, zero-based; `None` when the cursor is not viewport-resident (scrollback or hidden). |
| `lines` | `Vec<String>` | Viewport rows, top to bottom, right-trimmed. |
| `scrollback` | `Vec<String>` | History rows above the viewport, oldest first; empty unless requested. |
| `cells` | `Option<Vec<CellInfo>>` | Per-cell marks and styles; present only with `--cells`. |

**`scrollback` is tri-state** (mirrors [`mcp.md`](./mcp.md) §3.2): flag absent →
viewport only; `--scrollback` or `--scrollback=0` → all retained history;
`--scrollback N` → the most-recent `N` rows. On the wire this is `None` /
`Some(0)` (all) / `Some(n)`.

**`--cells`** populates `cells` with a sparse `Vec<CellInfo>` — only cells
carrying a non-default style or an OSC-133 mark, in row-major order, skipping
the right half of double-width glyphs. Each `CellInfo` is
`{ col, row, semantic?, style }`:

- `semantic` is `SemanticContent` — `Input` (typed input) or `Prompt` (shell
  prompt). `Output` is the default for every cell and is collapsed to absence,
  so `semantic` is `Some` only for marked input vs prompt.
- `style` is `CellStyle`: nine SGR booleans (`bold`, `faint`, `italic`,
  `underline`, `blink`, `inverse`, `invisible`, `strikethrough`, `overline`)
  plus `fg` / `bg`, each a `CellColor` tagged enum with `kind` of `default`,
  `palette` (`{ index }`), or `rgb` (`{ r, g, b }`). The tag distinguishes
  "terminal default" from "explicitly black".

**Back-compat.** `scrollback` and `cells` are `#[serde(default)]` (and `cells`
is `skip_serializing_if` `None`), so a `cells = None` snapshot serializes to
exactly the pre-cells shape, and an older consumer reading a newer payload
ignores extra keys. `schema_version` is the bump signal.

### 4.3 `RunResult` — `phux run --json` (on completion)

Defined in `crates/phux-client/src/run.rs`:

```json
{
  "command": "cargo test",
  "exit_code": 0,
  "output": "...",
  "duration_ms": 8123,
  "truncated": false
}
```

- `exit_code` (i32) is the child's `$?`, parsed out of a printed sentinel
  (`run` brackets the command with `BEGIN`/`RC` markers — it does not rely on
  shell integration).
- `output` is the rows between the `BEGIN` and `RC` markers.
- `duration_ms` (u64) is wall-clock from submit to sentinel-seen, including
  poll latency — an upper bound on the child's runtime, not a precise
  measurement.
- `truncated` is `true` when the `BEGIN` marker had scrolled out of the
  viewport, so `output` is best-effort visible context; a full capture needs
  scrollback.

**On timeout, `run --json` emits no JSON.** `RunOutcome::TimedOut` carries the
command, elapsed time, and last screen internally, but the CLI's `--json` path
serializes only the completed `RunResult`. The timeout signal is the exit code
(125 — see §5.2), printed alongside a stderr diagnostic. An agent must read the
exit code here and must not expect an `outcome: "timed_out"` body — that shape
exists in the MCP `phux_run` tool ([`mcp.md`](./mcp.md) §3.4), not in the CLI's
`--json` output.

### 4.4 `phux new --json`

`phux new --json -s NAME` emits a small fixed object naming the created session
and its seed pane's wire-local id, then exits `0` without attaching:

```json
{ "session": "NAME", "terminal_id": 2 }
```

It is create-only: `--json` requires an explicit `-s NAME` and errors (exit `1`)
if that name is already in use. Unlike the versioned `ScreenState` /
`RunResult` / `SessionListJson` shapes, this is a flat ad-hoc object with no
`schema_version`. The wire decomposition behind it is in §2.

### 4.5 Plugin registry — `phux plugin ... --json`

The plugin lifecycle surface is config-local. It edits or reads
`[[plugins]]` entries and validates referenced `phux-plugin.toml` manifests;
it does not load plugin code into phux and does not run plugin commands.

`phux plugin list --json` and `phux plugin validate --json` emit:

```json
{
  "schema_version": 1,
  "plugins": [
    {
      "id": "example.agent-tools",
      "name": "Agent Tools",
      "version": "0.1.0",
      "min_phux_version": "0.0.2",
      "description": null,
      "manifest": "./plugins/agent-tools/phux-plugin.toml",
      "manifest_path": "/abs/path/phux-plugin.toml",
      "plugin_root": "/abs/path",
      "enabled": true,
      "platforms": null,
      "build": [],
      "actions": [],
      "events": [],
      "panes": [],
      "links": []
    }
  ]
}
```

`validate --json` also carries `"valid": true`. `link`, `enable`, and
`disable` wrap the same plugin object under `"plugin"`; `unlink` wraps the
removed object under `"removed"`. The registry JSON enumerates declarative
actions, event hooks, pane providers, and link handlers from each manifest but
does not execute them. Invalid or missing manifests are hard failures: exit
nonzero, stdout empty, stderr diagnostic.

### 4.6 `ConfiguredAgentsJson` — `phux config agents --json`

`phux config agents --json` emits configured plugin agent declarations as a
consumer-ready list, merged with live runtime state when a server answers
(phux-r82.10). Schema history: version 1 was the pure manifest projection;
version 2 (current) makes `state`/`attention` the *effective* values —
runtime `phux.agent/v1` record first, declared manifest baseline as fallback
— and adds `live`, `source`, `declared`, and `runtime`:

```json
{
  "schema_version": 2,
  "live": true,
  "agents": [
    {
      "plugin_id": "example.agent-tools",
      "plugin_enabled": true,
      "id": "codex",
      "label": "Codex",
      "description": "Coding agent",
      "state": "blocked",
      "attention": "high",
      "source": "runtime",
      "declared": { "state": "working", "attention": "normal" },
      "runtime": {
        "terminal": "@3",
        "name": "codex",
        "kind": "codex",
        "state": "blocked",
        "attention": "high",
        "asked": false
      },
      "contexts": ["workspace", "pane"]
    }
  ]
}
```

`state` is one of `unknown`, `idle`, `working`, `blocked`, or (runtime only)
`done`. `attention` is one of `none`, `low`, `normal`, or `high`. `live` is
whether a server answered; with `live: false` every row is `source:
"manifest"`. `source` is `"runtime"` when a live `phux.agent/v1` record
matched the row (record `kind` slug, else lowercased `name`, equals the
agent id — the same identity derivation as `phux agent`), `"manifest"`
otherwise; `runtime` is `null` for manifest rows. When several panes declare
the same agent, the most attention-worthy binding is reported. Attention
follows the record's convention: declared value first, else derived from
state (blocked→high, working→normal, done/unknown→low, idle→none). An
active ADR-0035 ask on the matched pane sets `runtime.asked` and elevates a
record that declares *no* state to `blocked`; a declared record state
outranks the ask sentinel (ADR-0040). Invalid manifests are hard failures
and leave stdout empty on `--json`, preserving the script contract.

### 4.7 `AgentStateJson` — `phux agent ... --json`

`phux agent list --json`, `phux agent show --json [TARGET]`, and
`phux agent explain --json [TARGET]` emit the same versioned shape. `explain`
differs only in the human output; JSON always includes the evidence trail:

```json
{
  "schema_version": 1,
  "agents": [
    {
      "terminal": "@3",
      "session": "work",
      "window": "window-0",
      "agent": { "id": "codex", "label": "Codex", "kind": "codex" },
      "state": "blocked",
      "confidence": 0.95,
      "attention": "high",
      "title": "phux-ask[deploy]:Approve deploy??s=Yes|No",
      "cwd": "/repo",
      "sources": [
        {
          "kind": "title_ask",
          "signal": "phux-ask title sentinel",
          "confidence": 0.95,
          "observed": "phux-ask[deploy]:Approve deploy??s=Yes|No"
        }
      ],
      "explanation": "waiting on a reported human-answerable ask"
    }
  ]
}
```

`agent.kind` is `codex`, `claude`, `opencode`, `pi`, `omp`, `plugin`,
`declared`, or `unknown`. `state` is `unknown`, `idle`, `working`, `blocked`,
or `done`; `attention` is `none`, `low`, `normal`, or `high`. `sources` is
sorted by descending confidence and is the provenance contract: current
sources include `agent_record`, `title_ask`, `screen`, `semantic_cells`,
`identity`, and `plugin_report`. A structured `phux.agent/v1` record outranks
heuristics; without one, a plugin report remains lower precedence than a live
`phux-ask` title sentinel or an explicit blocked/completed screen cue.
Unknown/missing signals stay `unknown` or low-confidence `idle`.

This is a public clean-room projection. It does not copy external agent
manifests or private tradecraft rules; built-in recognition comes from
publicly observable process identity and captured pane chrome, plus optional
local phux plugin declarations.

### 4.8 `PluginActionOutput` — `phux config run --json`

Defined in `crates/phux-plugin/src/lib.rs` (`schema_version = 1`). Shape:

```json
{
  "schema_version": 1,
  "plugin_id": "example.agent-tools",
  "action_id": "summarize",
  "command": ["python3", "summarize.py"],
  "cwd": "/path/to/plugin",
  "outcome": "completed",
  "exit_code": 0,
  "stdout": "...",
  "stderr": "",
  "duration_ms": 42
}
```

`outcome` is `"completed"` or `"timed_out"`. `exit_code` is `null` when the OS
does not provide a process code or when phux kills the child on timeout. The
runtime executes the manifest's argv directly from the plugin root, captures
stdout/stderr lossily as UTF-8, inherits the phux process environment, and adds
`PHUX_PLUGIN_ID`, `PHUX_PLUGIN_ACTION_ID`, and `PHUX_PLUGIN_ROOT`.

### 4.9 Workspace commands — `phux workspace ...`

`phux workspace inspect --json` is repo-local. It shells out to git's porcelain worktree
listing and reports the current worktree plus siblings as a stable JSON
projection:

```json
{
  "schema_version": 1,
  "repo": {
    "path": "/abs/path/repo",
    "head": "012345...",
    "branch": "main",
    "detached": false
  },
  "worktrees": [
    {
      "path": "/abs/path/repo-feature",
      "head": "89abcd...",
      "branch": "feature",
      "detached": false,
      "current": false
    }
  ]
}
```

For detached worktrees, `branch` is `null` and `detached` is `true`. Missing
or non-git paths are hard failures: exit nonzero, stdout empty, stderr
diagnostic. The command is intentionally read-only; creation and deletion stay
in git/plugin/provider territory rather than the terminal substrate.

`phux workspace save` emits a separate archive shape:

```json
{
  "schema_version": 1,
  "sessions": [
    {
      "name": "agent-bench-codex",
      "active": true,
      "windows": [
        {
          "name": "0",
          "active": true,
          "panes": [
            {
              "active": true,
              "title": "codex",
              "cwd": "/repo",
              "command": null,
              "cols": 120,
              "rows": 40
            }
          ]
        }
      ]
    }
  ]
}
```

`command` is nullable because process argv is not always known. Plugin-authored
archives may fill it, and `workspace restore` uses it when present; otherwise it
starts the default shell in the saved cwd when available. Existing session names
are skipped, and restore prints a summary JSON document with `restored` and
`skipped_existing` arrays.

Restored sessions are fresh PTYs. The archive preserves window/pane metadata and
split-layout shape for inspection and future replay, but the current restore
command only recreates missing sessions and their seed process. Use `phux
upgrade` for live PTY handoff across a server re-exec; do not present workspace
restore as resurrecting already-running processes.

### 4.10 Satellite registry — `phux satellite ... --json`

The satellite lifecycle surface is config-local. It edits or reads
`[[satellites]]` entries and does not dial remote hosts.

`phux satellite list --json` emits:

```json
{
  "schema_version": 1,
  "satellites": [
    {
      "name": "devbox",
      "endpoint": "ssh://devbox",
      "enabled": true
    }
  ]
}
```

`add --json` wraps the same satellite object under `"satellite"`; `remove
--json` wraps the removed object under `"removed"`. Invalid names, invalid
endpoint URIs, duplicate configured names, and refused registry writes are hard
failures: exit nonzero, stdout empty, stderr diagnostic.

### 4.11 `phux spawn --json`

`phux spawn --json` emits a small fixed object naming the spawned terminal:

```json
{
  "terminal_id": 7,
  "satellite": null
}
```

`satellite` is the registry name when the spawn was routed with
`--satellite NAME` (in which case `terminal_id` is the id *on that
satellite* — address the pane through the hub by the pair), and `null`
for a local spawn (address it as `@7`). Failures — no route to the named
satellite, unreachable satellite link, server-side spawn failure — exit
nonzero with stdout empty and the typed diagnostic on stderr.

### 4.12 Spatial layout edits

Each successful `--json` spatial edit emits a `schema_version: 1` document.
Common fields are `operation` and `session_id`; insert adds
`target_terminal_id`, `new_terminal_id`, `direction`, and `ratio`; move adds
`source_terminal_id`, `target_terminal_id`, `direction`, and `ratio`; swap adds
`first_terminal_id` and `second_terminal_id`. `direction` retains the CLI's
user-facing divider meaning (`vertical` = side-by-side, `horizontal` = stacked),
not the layout tree's internal child-axis enum.

```json
{
  "schema_version": 1,
  "operation": "insert-pane",
  "session_id": 3,
  "target_terminal_id": 7,
  "new_terminal_id": 9,
  "direction": "vertical",
  "ratio": 0.3
}
```

With `--json`, failures emit a versioned JSON error on stderr and leave stdout
empty: `{ "schema_version": 1, "error": { "code": "...", "message": "..." } }`.
Stable codes include `invalid_selector`, `selector_miss`,
`selector_not_single`, `satellite_target`, `cross_session`, `invalid_ratio`,
`layout_missing`, `pane_not_in_layout`, and `pane_already_in_layout`.

### 4.13 `phux launch --json`

A successful launch returns the resolved integration identity, final argv, and
new local terminal id:

```json
{
  "schema_version": 1,
  "terminal_id": 11,
  "integration": "codex",
  "plugin": "com.phux.agent-tools",
  "argv": ["phux-agent-wrap.sh", "codex"]
}
```

`phux launch --list --json` instead returns
`{ "schema_version": 1, "integrations": [...] }`; `--print --json` returns the
resolved `cwd`, `working_directory`, and `argv` without spawning. Placement does
not add a second result shape: `--target`, `--split`, and `--ratio` affect the
persisted topology while the launch JSON remains the object above.

### 4.14 `phux rec --json`

One object on stdout on success, and nothing else on stdout ever. Progress and
every diagnostic go to stderr, and progress is suppressed entirely under
`--json`:

```json
{
  "path": "demo.gif",
  "format": "gif",
  "bytes": 188742,
  "duration_ms": 42130,
  "frames": 211,
  "cols": 120,
  "rows": 34,
  "truncated": false
}
```

`format` is one of `cast`, `gif`, `apng`. `duration_ms` is the recording's own
timeline after the idle clamp, not wall time spent recording. `frames` is the
count of encoded animation frames — for `format: "cast"` there are no frames,
so it reports the cast's event count instead. `cols`/`rows` are the recorded
grid. `truncated` is `true` when encoding stopped at `--max-bytes`: the file is
a complete, playable container, just shorter than the capture.

**Divergence from §4:** this object carries no `schema_version`. It is a
result line rather than a projection of engine state, and it has no producing
struct in `phux-core`. Read the keys, not a version.

Exit codes: `0` on success, including a capture you ended with Ctrl-C; `1` on
failure (no server, unresolvable target, unknown output extension, unreadable
`--from` file, write or encode failure). A failed *export* is still exit `1`,
but the captured `.cast` is deliberately retained and its path printed, so the
recovery is `phux rec --from <that path> -o <target>`.

### 4.15 `phux resize --json`

A `schema_version: 1` object naming what was asked for and what the server
actually holds afterwards:

```json
{
  "schema_version": 1,
  "terminal_id": 7,
  "requested": { "cols": 120, "rows": 40 },
  "applied": { "cols": 120, "rows": 40 },
  "held": true
}
```

`applied` is read back from the server, not echoed from the request, so it is
the geometry a following `phux snapshot` will report. `held` is `applied ==
requested` on both axes and mirrors the exit code, so a script can branch on
either. Without `--json` the same fact prints as one line, `120x40` — the
applied size, so `phux resize demo 120x40` is safe to read with `$(...)`.

**Divergence from the other `--json` verbs:** the object is printed on the
failure path too, and stdout is not left empty. Elsewhere a nonzero exit means
the command did not run; here it means the command ran and the server holds a
different size, and that size is exactly what the caller needs in order to
react. Transport failures — no server, unresolvable target — do leave stdout
empty, as everywhere else.

## 5. The read-act-wait loop and exit-code mirroring

### 5.1 The loop

The single-pane pattern is read → act → wait → read: snapshot the pane, send
input or run a command, wait for the result to land, snapshot again. Every wait
must carry a finite timeout; a CLI `watch` is an unbounded stream and must run
under a child-process deadline. A worked example in `sh`:

```sh
phux send-keys build "cargo test" Enter
phux wait --until "test result:" --timeout 120 build
phux snapshot --json --scrollback 200 build > out.json
```

When you only want a command's exit code and output, the one-shot `phux run` is
the higher-level alternative — it brackets the command with sentinels and
mirrors `$?`:

```sh
phux run --json build "cargo test"
```

The contrast: `run` is "I want the exit code"; `send-keys` plus `wait` is "I am
driving an interactive or long-lived program." Because `run` mirrors the
child's code (§5.2), `phux run ... && next` composes like a shell
([ADR-0022](../../ADR/0022-tool-for-agents.md) §3).

When the input itself is a block of text — a heredoc body, an indented code
snippet for a REPL, a multiline SQL statement — use `paste`, then submit
explicitly:

```sh
phux paste repl "$(cat snippet.py)"
phux send-keys repl Enter
```

`send-keys` without a trailing `Enter` would type the block character by
character, letting the REPL's auto-indent mangle every indented line; `paste`
delivers it as one bracketed block (when the pane's program supports DEC mode
2004) and the program inserts it verbatim, waiting for the explicit Enter.

The fleet extension is discover → create → place → shape → act → observe →
surface asks → verify. See the executable
[`examples/agents/orchestrate-placed-fleet`](../../examples/agents/orchestrate-placed-fleet):
it launches/spawns with explicit placement, serializes topology edits, watches
agent panes concurrently under hard bounds, and prints `C-a q` / `C-a Q` human
guidance without changing focus.

### 5.2 Exit-code mirroring

Exit codes are not uniform across verbs:

| Verb | Exit codes |
|---|---|
| `ls` | `0` ok; `1` no server / unexpected result. |
| `snapshot` | `0` ok; `1` failure (no server, serialize error, resolve miss). |
| `send-keys` | `0` ok; `1` failure (no server / refused / miss). |
| `paste` | `0` ok (including a paste the pane's untrusted policy silently dropped); `1` failure (no server / refused / miss / unreadable stdin). |
| `ask` | `0` accepted; `1` no server, unknown pane, or invalid ask payload. |
| `agent` | `0` ok; `1` no server, unknown pane, or JSON render failure; `3` the miss is not trustworthy — see below (`show`/`explain`/`set`/`clear`; `list` enumerates and stays `0`). |
| `run` | the child's own code clamped to `0..=255` (negative or `>255` saturate to `255`); `125` when phux gave up waiting for the sentinel (`--timeout`); `1` for no server / refused target / other. |
| `wait` | `0` condition met; `124` on `--timeout`; `1` no server / parse / read error. |
| `new` | `0` ok; `1` duplicate `-s` name / failure. |
| `resize` | `0` the pane holds the requested geometry; `1` no server / selector miss / unknown pane, or the server holds a different size (an attached view's `window-size` policy owns it); `2` unusable `COLSxROWS` (clap usage error, raised before any connection). |
| `rename` | `0` renamed; `1` no server or transport failure; `2` unknown source session or destination name already exists. |
| `launch` / `spawn` | `0` spawned/resolved/listed; `1` invalid integration, placement, server, or spawn failure. |
| `watch` | streams until Ctrl-C, EOF, or caller termination; use a subprocess bound rather than treating its eventual signal status as agent outcome. |
| `rec` | `0` ok, including a capture ended by Ctrl-C; `1` no server, unresolvable target, unknown output extension, unreadable `--from` file, or write/encode failure. |
| `plugin` | `0` ok; `1` invalid/missing manifest, invalid config, refused registry write, or unknown plugin id. |
| `workspace` | `0` ok; `1` missing git repo, invalid git output, no server for save/restore, invalid archive, or JSON render failure. |
| `satellite` | `0` ok; `1` invalid name/endpoint, duplicate configured name, invalid config, refused registry write, or unknown satellite name. |
| `insert-pane` / `move-pane` / `swap-pane` | `0` ok; `1` transport failure; `2` selector, ratio, session, or layout refusal. |
| `kill` | `0` ok; `1` selector miss / no server / parse; `2` server-side refusal; `3` the miss is not trustworthy — see below. |
| `tag` | `0` ok; `1` selector miss / no server / invalid target; `3` the miss is not trustworthy — see below. |

**Exit `3` — "I could not answer, because I could not see all of the fleet."**
A federation hub that cannot reach a satellite still answers `GET_STATE`, with
that satellite's panes simply missing from the merge. Every `TARGET` selector
is a *search* over those panes, so a search that finds nothing has two causes
that must not share a sentence or a status: the target does not exist (`1`,
`no such target: X`), or the server could not look where it lives (`3`, a
message naming the unreachable satellite and containing neither the words
"no such target" nor any claim of absence). Retrying is the right response to
`3` and the wrong response to `1` — that is why they are different numbers.
`kill`, `tag`, and `agent show`/`explain`/`set`/`clear` return it.

**Every** target-resolving verb prints the distinguished message, but not all
of them can spend a status on it. `snapshot`, `send-keys`, `paste`, `run`,
`wait`, `watch`, `resize`, `signal`, `rec`, and `ask` share one resolver, and
two of them have already spoken for the number: `run` mirrors the child's own
exit code (a command may legitimately exit `3`) and `wait` owns `124`. Those
verbs keep `1` and say it in words. Branch on the status where the table below
offers `3`; otherwise read stderr, which never claims absence it cannot
verify.

Verbs that resolve a **session name** never return `3`: a satellite's
`sessions` and `windows` lists are discarded during the merge (their ids would
collide with the hub's), so the session name space is complete even when the
fleet is not. `rename` therefore keeps its confident `2` for an unknown
session, and warns on stderr about the outage without changing its answer.
Verbs that *enumerate* (`ls`, `agent list`) warn on stderr and exit `0`; under
`--json`, `ls` reports it structurally in `unreachable` (§4.1) because a
`--json` consumer does not read stderr.

**Why `run` uses 125, not 124.** `run` mirrors the child's own code into
`0..=255`, and `124` is a code real commands produce — notably GNU `timeout`.
So `run` reserves `125` (the wrapper-failure convention, as used by `env` and
`timeout`) for "phux itself gave up," keeping it distinct from a child that
legitimately exited `124`. `wait`, which wraps nothing, uses `124` for its own
timeout. `kill` is a control-plane verb (not strictly an agent read) but shares
`TARGET`; its `0`/`1`/`2` triad is listed for completeness.

## 6. Relationship to the other agent surfaces

The CLI verbs here are the stable contract. The
[OpenCode integration](./opencode.md) selects a host-specific six-tool subset;
the [Pi integration](./pi.md) exposes nineteen bounded tools, including spatial
placement and topology edits. The [MCP adapter](./mcp.md) exposes 22 strict
tools, including paste, launch/spawn, bounded watch, ask, spatial edits, agent
state, and workspace parity, over JSON-RPC stdio. Adapter guides link here instead of
redefining CLI syntax.
[`sdk.md`](./sdk.md) documents `phux-client`, the library crate those surfaces
are built from. These adapters are unprivileged consumers
([ADR-0017](../../ADR/0017-tui-not-protocol-privileged.md)); the wire
underneath stays additive and versioned under [`../spec/`](../spec/)
([ADR-0022](../../ADR/0022-tool-for-agents.md)).
