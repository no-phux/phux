---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-08-09
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
normative under [`../spec/`](../spec/). Installing the MCP companion does not make its
tools visible to a host; see [Registering with a host](./mcp.md#registering-with-a-host)
for the Claude Code command and generic stdio configuration.

The selector grammar is owned by [`tui.md`](./tui.md) §3; this file links there
rather than restating the table (the doc system's one-fact-one-home rule). The
decision rationale lives in
[ADR-0022](../../ADR/0022-tool-for-agents.md); client-side selector resolution
in [ADR-0021](../../ADR/0021-control-plane-commands.md).

**Viewport-safe against a live pane.** `snapshot`, `run`, `send-keys`,
`paste`, `wait`, `agent wait`, and `agent send-keys` neither attach nor resize
the target pane: reads issue `GET_SCREEN` or `GET_METADATA`, and input rides
`ROUTE_INPUT` to a pane id. `snapshot`/`wait`/`agent wait` are
side-effect-free; `run`/`send-keys`/`paste`/`agent send-keys` deliberately
mutate the live PTY. `snapshot --tail` and `--unwrap`, and `wait --tail` /
`--regex` / `--output-only`, are client-side projections over that same read
and change nothing about it. None changes an attached human's local focus or
viewport. `resize` is
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
- **`phux snapshot [--json] [--scrollback[=N]] [--cells] [--tail[=N]]
  [--unwrap] [--socket P] [TARGET]`** — side-effect-free pane read via
  `GET_SCREEN`. `TARGET` is optional (defaults to the focused session).
  `--json` emits `ScreenState` (§4.2); without it, a boxed text view.

  `--tail[=N]` returns the last N rendered rows — history above the viewport,
  then the viewport, oldest first. Bare `--tail` is 80 rows; `--tail 0` is all
  retained history, capped at 10 000. **The viewport is a floor and is never
  returned in part**, because `rows`, `cursor`, and `cells` are grid
  coordinates and a partial grid would lie about them; a window narrower than
  the viewport therefore returns more rows than you asked for, never fewer.
  Dropped rows set `truncated` (§4.2).

  `--unwrap` joins soft-wrapped rows into logical lines — rows as *written*
  rather than as *painted*. It cannot be combined with `--cells`, whose
  coordinates do not survive the join, and after it `soft_wrap` reports no
  wrapped rows because the returned projection has none.

  Both are client-side projections of the same side-effect-free `GET_SCREEN`:
  neither adds a wire field, neither moves a live pane, and §1's
  viewport-safety claim for `snapshot` is unchanged.
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
  content you did not compose and cannot vouch for. One `paste` call is one
  atomic `INPUT_PASTE` event; there is no guarantee across separate
  fire-and-forget calls, so never split one logical payload (one command,
  one file body) across multiple `paste`/`send-keys` invocations expecting it
  to arrive whole — see [input.md §5.1](../spec/input.md) for why an
  interior drop is worse than a whole-event drop, and use `APPLY_INPUT` or
  `PUT_FILE` at the protocol layer when a payload cannot fit in one event.
- **`phux run [--timeout SECS] [--json] [--socket P] TARGET CMD...`** — run a
  command in a pane and capture its exit code, output, and duration via printed
  sentinels (assumes a POSIX shell: sh/bash/zsh). `TARGET` is required.
  `--json` emits `RunResult` (§4.3). The exit code mirrors the child (§5.2).
  Flags must precede `TARGET`, or clap's `trailing_var_arg` swallows them into the
  command line.
- **`phux wait [--until TEXT] [--regex PATTERN] [--idle MS] [--tail[=N]]
  [--output-only] [--timeout SECS] [--json] [--socket P] [TARGET]`** — poll the
  side-effect-free screen read until a condition holds. A text condition
  (`--until` or `--regex`, mutually exclusive) takes precedence over `--idle`;
  with none of them it settles on idle. `--json` emits the final `ScreenState`
  as read — the projections below scope the *match*, not the emitted document.
  Exit 0 when the condition is met, 124 on timeout.

  **Matching is against the lines as written, not as painted.** Rows the
  terminal soft-wrapped at its right edge are joined into logical lines first,
  so a needle that straddles a wrap is found. This is a behavior fix rather
  than a flag: before it, `--until` on text that happened to fall across the
  wrap silently never matched and the wait ran to its timeout, which looked
  like a hang and appeared only for long lines. Against a server that does not
  report wrap bits (`soft_wrap` absent, §4.2) rows are matched verbatim — the
  old behavior, detectable rather than guessed at.

  `--regex PATTERN` is a Rust regular expression matched one logical line at a
  time, so `^` and `$` anchor to a line you can see and no pattern spans two of
  them. An invalid pattern is a usage error (exit 2) raised while parsing the
  command line, before any connection or poll, rather than a wait that quietly
  never matches.

  `--tail[=N]` scopes matching to the last N logical lines and reads that much
  history to do it; without it only the viewport is read, as before. Bare
  `--tail` is 80, `--tail 0` is all retained history capped at 10 000. Three
  things to know about the count: it is over logical lines, after wrapped rows
  are joined, so it can never bisect a wrapped run; it ignores the blank rows
  under the cursor, because a grid is always full height and counting them
  would make a small window match nothing on a pane whose prompt sits near the
  top; and unlike `snapshot --tail` the viewport is **not** a floor — this
  window scopes a search rather than describing a returned grid, so `--tail 3`
  really does mean three lines. It counts the prompt block already back on
  screen, so leave room for it. `--tail` also narrows `--idle`: only those
  lines have to hold still, which is one way to settle a pane with a spinner
  further up.

  `--output-only` drops lines the shell marked as your own typed input
  (OSC-133 `Input`, the same marks `snapshot --cells` exposes), so a wait
  cannot be satisfied by the echo of the command that started the work. A
  command too long for one row is dropped whole; prompt-marked text is kept;
  history rows carry no marks and are always treated as output. It needs a
  shell with OSC-133 integration — with no marks at all it filters nothing and
  phux says so on stderr before the wait begins, because failing closed would
  hang a wait that is otherwise fine.

  The gotchas that remain: flags must precede `TARGET`; a bare `--tail` reads
  the next word as N, so spell N out when you also pass a target (`--tail 80
  build`, not `--tail build`, which is a usage error); and **without
  `--output-only`, `--until`/`--regex` still match any line including the
  shell's echo of the command you just typed** — on a pane with no shell
  integration, match on text that appears only in command output, never the
  command itself.
- **`phux watch [--until EVENT]... [--timeout SECS] [--json] [--socket P]
  [TARGET]`** — stream a pane's live events
  (the push half of the agent surface; see [`../spec/L1.md`](../spec/L1.md)).
  Subscribes to the server's event stream scoped to the resolved pane and
  prints one event per line until EOF (server gone) or Ctrl-C; the
  subscription neither attaches nor resizes the pane. With `--json`, each line
  is a JSON object `{ "event": <name>, "terminal"?: "@id", ... }` and stdout
  stays pure JSON (diagnostics on stderr); otherwise a compact tab-separated
  human line. Event names: `title_changed` (carries `title`), `bell`, `dirty`,
  `idle`, `pane_spawned`, `pane_closed` (carries `exit_status`), `asked`
  (carries `id`, `question`, `suggestions`, and nullable `elapsed_seconds`),
  `command_started` / `command_finished` (the latter carries a nullable
  `exit_code`), and `agent_state`.

  Repeating `--until` turns the stream into a gate: it prints through the
  first matching event and exits `0`. Accepted names are `agent_state`,
  `asked`, `bell`, `command_finished`, `command_started`, `dirty`, `idle`,
  `pane_closed`, `pane_spawned`, `title_changed`, and `unknown`; an unknown
  name is refused before connecting. `--timeout` bounds either a gate or a
  plain stream and exits `124` without appending a summary to the NDJSON.
  Server EOF before an `--until` match exits `1`; Ctrl-C remains a clean `0`.

  `agent_state` is the derived-agent half of the stream, not an L1 event:
  `watch` also subscribes to the resolved pane's
  `phux.agent/v1` L3 record ([`../spec/L3.md`](../spec/L3.md) §3.7), so every
  transition the server-side detector publishes
  ([ADR-0046](../../ADR/0046-server-side-agent-state-detection.md)) arrives as
  a line. It carries `name`, optional `kind` and `session`, `state`,
  `attention`, and `from` — the state the same pane was last seen in *within
  this `watch` run*, absent on the first record for a pane. `attention` is the
  effective level L3 §3.7 derives from `state`, because the detector never
  writes the field itself. A deleted record (or a value that is not a readable
  record, which L3 §3.7 reads as "no declared agent") emits a line with `state`
  present and `null`, keeping `name` and `from` from the last record seen,
  rather than being dropped: a consumer waiting on an agent has to learn that
  it went away. The subscription is one `(scope, key)` pair against the
  resolved pane, so `agent_state` lines are always scoped to that pane; there
  is no fleet-wide agent-state stream, because L3 has no wildcard-Terminal
  scope. Watch the panes you care about, one stream each.

  `watch` cuts `wait`'s poll-floor latency: a `watch` consumer
  wakes the instant an event fires rather than on the next poll tick. It is
  additive — `wait` still works without it, and a dropped event (full
  mailbox) falls back to polling.

  **No `schema_version` on this stream, by design (ADR-0071).** Every other
  `--json` document in this catalog stamps a `schema_version`; this NDJSON
  stream deliberately does not, on either of the two shapes you might expect
  instead — a per-line field (repeated overhead on a hot, high-volume path,
  for a value that essentially never changes mid-run) or a versioned header
  line (invisible to the consumer shape this stream actually has: one that
  attaches, disconnects, reconnects, or `tail`s an existing pipe, and so may
  never see line one). The stream is versioned by the *binary* (`phux
  --version`), and the compatibility unit is the `event` name vocabulary
  above: a consumer ignores an `event` value or a field it does not
  recognize, the same way the reference decoder already renders an unknown
  wire event tag generically instead of failing the stream. A shape-breaking
  change to an existing event is a breaking change to the CLI's frozen JSON
  surface exactly like any other and requires the major version bump
  ADR-0071 already mandates for one — nothing here is exempt, only unmarked
  per line.
  **Command boundaries (phux-foz.4):** `command_started` / `command_finished`
  are sourced from a direct scan of the raw PTY byte stream for `OSC 133 ; C`
  / `OSC 133 ; D` shell-integration marks — `C` emits `command_started`, `D`
  emits `command_finished` with the shell-reported exit code in `exit_code`
  when the mark carries one (`OSC 133 ; D ; n`). `exit_code` is `null` only
  when the shell's `D` mark omits the code or the shell has no OSC-133
  integration at all; it is not always null. A pane whose shell never emits
  OSC-133 marks (no shell integration configured) never produces these two
  events — `dirty`/`idle` remain the fallback command-boundary signal in that
  case. **Remaining caveat:** `idle` (and by extension the fallback boundary)
  depends on the PTY actually going quiet; a live shell prompt whose program
  keeps repainting (for example a blinking cursor or a redrawing status line)
  can keep generating chunks that never settle, so idle cadence is only as
  good as the prompt's own quiescence.
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
- **`phux play FILE [TARGET] [--speed N] [--idle-limit SECS] [--loop [N]]
  [--no-fit] [--close] [--split AXIS] [--ratio F] [--json] [--socket P]`** —
  create a pane whose PTY is fed from a recording, and print its Terminal id.
  The pane is an ordinary one: `snapshot` it, `resize` it, `rec` it, `watch`
  it, `kill` it. TARGET names the pane the new one is placed *beside* (default
  `.`) and is never written to — there is no way to play into a pane that
  already has a shell. The verb returns as soon as the pane exists; it does
  not block for the length of the recording, so an agent that wants the final
  screen should poll `phux snapshot` rather than wait on this process. The
  pane holds its final frame until killed unless `--close` is given, which is
  what makes that poll safe. `--json` emits the result object (§4.16). Full
  surface in [`recording.md`](./recording.md) §6.
- **`phux ask TARGET [--id ID] [--suggest TEXT...] [--elapsed-seconds SECS]
  [--json] [--socket P] QUESTION`** — report that an agent in a pane is blocked
  on a human-answerable question. This is the opt-in hook ingress from
  ADR-0036: configured plugin actions or first-party integrations call it
  instead of writing a `phux-ask` title sentinel themselves. It resolves
  `TARGET` client-side, does not attach or resize, and asks the server to emit
  the normal `asked` event on the existing watch stream. `--json` echoes the
  reported `{ schema_version, event, terminal, id, question, suggestions,
  elapsed_seconds }` object after the server accepts the payload. Empty
  questions, empty suggestions, excessive suggestion counts, and unknown panes fail without
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
- **`phux agent explain --file PATH --kind KIND [--title TEXT]
  [--format auto|json|text] [--json]`** — the offline half of `explain`. It
  evaluates the compiled detection manifests
  ([ADR-0046](../../ADR/0046-server-side-agent-state-detection.md)) against a
  captured screen and contacts no server at all; `--file -` reads stdin.
  `PATH` is `phux snapshot --json` output or a plain text screen, one viewport
  row per line, and `--format auto` (the default) picks JSON when the first
  non-whitespace byte is `{`. `--kind` is required and takes a kind slug or
  one of its binary aliases (`claude-code` resolves to `claude`): offline
  there is no foreground process group to identify the agent from, so a miss
  enumerates the loaded manifests rather than guessing. A capture carries no
  OSC title, so `--title` supplies one for title-scoped rules; without it
  every `title` rule reads an empty region, and the report says so. The output
  is the evidence, not the answer: the text every region resolved to on that
  screen, then every rule — matched and unmatched — with its predicate tree
  annotated node by node. A rule scoped to a region that comes back empty
  cannot match however well it is written, and because the detector fails safe
  to `idle`, nothing else makes that visible. `--file` conflicts with
  `TARGET`; `--kind`, `--title`, and `--format` require `--file`. `--json`
  emits `AgentExplainJson` (§4.7).
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

  **A declared `state` outranks the detector only while the pane is still
  occupied by the agent it describes.** Omitting `--state` writes the literal
  `"unknown"`, which is *not* a declaration: the record supplies identity and
  the detector fills `state` in, preserving your `name`, `kind`, and
  `session`. Supplying any other `--state` stands the detector's derivation
  down on that pane — deliberately, because a lifecycle hook is better
  evidence than a screen rule ([ADR-0046](../../ADR/0046-server-side-agent-state-detection.md)
  point 8). The declaration then ends in one of three ways: `agent clear`
  (or any `DELETE_METADATA`), the pane being reaped, or the server
  **withdrawing** it. A withdrawal is what happens when the declaring process
  dies without clearing — a `SIGKILL`, a force-closed pane, any exit that
  skips the integration's cleanup path. On positive evidence that the pane's
  occupant is gone or has changed, the server sets `state` to `"unknown"` and
  clears `attention`, and stops there: it never substitutes a derived value
  for your declaration and never deletes a record it did not author
  ([`docs/spec/L3.md`](../spec/L3.md) §3.7, normative). Your `name`, `kind`,
  and `session` survive untouched; only the claim about lifecycle is dropped,
  and the derivation resumes from there. *Positive evidence* is an
  observation the server successfully made and which found no such agent — a
  server that cannot see the pane's foreground process holds the declaration
  rather than guessing, so a declared record is never withdrawn by a failed
  query. Consumers see this as an ordinary transition into `unknown` (§5.1),
  which is a *departure*, never a completion.
- **`phux agent clear [TARGET] [--socket P]`** — delete the declared record
  (`DELETE_METADATA`); consumers fall back to the OSC-title and screen
  heuristics. Prints `@N<TAB>-` on confirmation. This is the only verb that
  removes the record; the withdrawal described above empties the state and
  keeps the identity, so a withdrawn pane still resolves by name and kind.
- **`phux agent wait [--until STATE]... [--timeout SECS] [--json] [--socket P]
  [TARGET]`** — block until the pane's agent **transitions into** a lifecycle
  state. `--until` repeats and ORs over `idle`, `working`, `blocked`, `done`,
  defaulting to `idle,blocked,done` — the three ways a turn ends. Detection
  manifests cannot honestly derive `done` from a screen, but the bundled
  Claude shim reports the Stop hook through `REPORT_AGENT_STATE`; therefore
  `--until done` is meaningful on an instrumented Claude pane. Other agents
  need their own lifecycle integration and may otherwise time out.
  `unknown` is not spellable: it is *departure*, not a state to wait for.
  A **satellite `TARGET` is refused** (`satellite_target`, exit 2) as soon as
  the selector resolves, before the wait subscribes to anything:
  `phux.agent/v1` is hub-local, so a hub has no record for a remote pane and
  can never be told one changed. Run the wait on the
  satellite's own server. `phux watch` still carries that pane's agent
  *events* across the hub — it is the metadata half that does not federate,
  not the event half.
  `TARGET` is optional (the focused pane). `--timeout` is in seconds and is
  unbounded when omitted, matching `phux wait`; always pass one from a
  script. `--json` emits
  `AgentWaitJson` (§4.7).

  **It is satisfied only by an observed transition, never by a level read of
  the current state**, for the reason [`../spec/L3.md`](../spec/L3.md) §3.7
  states normatively: a level read of `state` asserts only that nothing
  contrary is being asserted, and `idle` is the weakest value in the
  vocabulary — normally the reference detector's fail-safe fallthrough
  ([ADR-0046](../../ADR/0046-server-side-agent-state-detection.md)). Claude's
  captured OSC 9;4 remove signal is one positive-idle source, but a level read
  still says nothing about whether that transition occurred after this wait's
  baseline. A completion gate that fired on a level would also report success
  instantly on crashed panes and panes with no manifest. §5.1 has the loop.

  The verb subscribes to the pane's `phux.agent/v1` key **before** reading the
  pre-wait baseline, on one connection, so no transition falls in the gap; it
  also re-reads `GET_METADATA` on the `phux wait` cadence, because the change
  notification is droppable and the detector is edge-filtered. That re-read is
  level-triggered *recovery of an edge* — it goes through the same
  must-have-changed rule — never a level gate. **The deliberate consequence:**
  a pane already resting in a target state when the wait begins times out
  rather than succeeding, and the timeout diagnostic names the state it held
  so one `phux agent show` recovers. That is a loud false negative in place of
  a silent false positive.
- **`phux agent send-keys [--expect-agent NAME] [--expect-kind KIND] [--json]
  [--socket P] TARGET KEYS...`** — the agent-addressed sibling of
  `phux send-keys`, differing from it in exactly one way: it re-reads the
  pane's `phux.agent/v1` record immediately before writing and refuses if the
  occupant is not the agent you named. `phux send-keys` addresses a *pane* and
  deliberately checks no identity — use that one when a pane is what you mean.
  A pane with no record is refused rather than written to.

  **`--expect-agent` matches `name`, and a detector-written `name` is a
  per-kind constant, not a per-pane label.** A detection manifest is written
  once per agent *kind*, so every pane the detector recognizes as Claude
  carries `name = "claude"`, and so does every pane running the hook shim.
  `--expect-agent claude` therefore asserts "a Claude is in this pane", which
  is a real and useful check but not an identity — it passes on any of twelve
  Claude panes. The detector will not invent `claude-7` to paper over this:
  the pane id *is* the per-pane identity and it is already the selector you
  used. If you want a name that distinguishes one pane from another, set it
  yourself with `phux agent set @7 --name reviewer`; an explicitly set `name`
  is never overwritten by the detector, and `--expect-agent reviewer` then
  means what it looks like. Use `--expect-kind` when the kind is what you
  actually care about.

  Every key spec is validated before any byte is written, so a typo in the
  third key cannot leave the first two delivered; unlike `phux send-keys`, a
  near-miss chord (`C-cc`, a bare `M-`) is refused rather than typed as
  literal text. The identity read and acknowledged `APPLY_INPUT` are ordered
  on one connection. Success means `write_all` plus `flush` completed on the
  PTY master: the bytes reached the kernel tty queue, not necessarily the
  agent. `INPUT_DELIVERY_UNKNOWN` is terminal; read the pane and do not resend
  under a new operation id. The server currently has one acknowledged input
  lane, so serialize concurrent acknowledged writes.

  What the check *can* now rely on is that a stale `kind` does not sit beside
  a live `state`. When the pane's occupant changes — a Claude killed and a
  Codex started in the same pane, or the same kind restarted as a new process
  — the server corrects `kind` and drops `state` to `"unknown"` in one write,
  rather than letting the new occupant's derived state accumulate under the
  old occupant's label. A record that reads `kind: claude, state: working` is
  therefore evidence about a live Claude, not a leftover. The exception is a
  `kind` you set explicitly: the server preserves an explicit writer's `kind`
  (§3.7 of the spec requires it), so if you declared one, keeping it accurate
  is yours to do. `--json` emits the shape in §4.7.

- **`phux agent prompt [--expect-agent NAME] [--expect-kind KIND] [--wait]
  [--until STATE]... [--timeout SECS] [--json] [--socket P] TARGET TEXT`** —
  submit one single-line prompt plus Enter as one acknowledged, idempotent
  operation. Raw newlines are refused. `--wait` holds the same process and
  connection across delivery and waits only for a post-write transition, so
  the fused form cannot miss a fast turn between two commands. `--until` and
  `--timeout` require `--wait`; states default to `idle,blocked,done`. Timeout
  exits `124` after reporting that delivery occurred. The acknowledged input
  lane is per server, with one admission slot and one execution thread;
  prompt fleets serially. An OK is a kernel-queue receipt, not proof of
  consumption. If delivery is unknown, inspect the pane and do not resend.
- **`phux agent answer --id ID (--choice N|--text TEXT)
  [--allow-unlisted] [--json] [--socket P] TARGET`** — answer the exact ask
  still live on the pane. `--choice` is one-based into the published
  suggestions. Free text must equal a suggestion unless `--allow-unlisted`
  is explicit. A stale id, anonymous ask, or pane no longer asking is refused
  with nothing written; a valid answer is one acknowledged paste-plus-Enter
  operation.
- **`phux agent start --kind KIND --target TARGET [--integration ID]
  [--timeout SECS] [--no-wait] [--force] [--json] NAME [-- ARGS...]`** —
  start an agent inside an existing shell pane. It never creates, splits,
  moves, or focuses layout. `--integration` defaults to the unique enabled
  integration whose `[agent_identity] kind` matches `--kind` (so
  `--kind claude` starts `claude-code` with no second flag), falling back
  to the kind slug itself; two enabled integrations claiming one kind are
  refused by name (`ambiguous_integration`) rather than picked between,
  and the explicit flag remains the override. Without `--no-wait`, success
  requires the first
  detector publication after submit; a kind with no manifest is refused
  because readiness would be unenforceable. Timeout exits `124` after the
  command was typed. Before writing, the verb reads the server-owned
  `phux.pane-occupant/v1` record: a foreground process other than the pane's
  original shell is refused, while a confirmed pane shell works even without
  OSC-133 shell integration. A contradictory OSC-133 busy mark still refuses
  because the process observation is periodic. `--force` skips only this
  available-shell precondition.

- **`phux agent install-claude [--shell zsh|bash|fish] [--real PATH]`** —
  make plain interactive `claude` invocations enter phux automatically. The
  installer leaves the real Claude binary untouched, writes a phux-owned shim
  under `$XDG_DATA_HOME/phux/shims`, and adds one marked PATH block to the
  detected shell rc. Outside phux, the shim creates and attaches a new session
  in the caller's working directory; inside a pane it runs Claude in place.
  Noninteractive/admin invocations such as `claude -p`, `claude mcp`, and
  `claude --version` bypass phux.

  **Only the session-start hook writes the record, and it declares identity
  only — `--name claude --kind claude`, never a `--state`.** Per-turn hooks
  feed `working`, `blocked`, and `done` to the server detector with `phux agent
  report-state`; they do not write metadata. A hook that declared a
  `state` would stand the ADR-0046 detector down on that pane for the record's
  lifetime, and `claude.toml` is the deepest manifest phux ships. The
  per-turn hooks write nothing to the record because an identity write also
  replaces the derived `state` (§3.7 records are replaced wholesale, not
  merged), which a repeated write turns into a false departure edge.
  The detector publishes hook evidence immediately, then resumes ordinary
  screen derivation, so a missed cleanup hook cannot latch stale state. Blocked
  notifications still emit `phux ask`, so phone and TUI fleet
  views see attention without screen inference; that path is unchanged and
  keeps the hook's exact timing. The Stop hook is again an honest `done`
  producer, so `agent wait --until done` is satisfiable on shim panes.

  The installed shim is **version-stamped** (`# phux-shim-schema: N` on the
  second line). `install-claude` reports `installed`, `reinstalled`, or
  `upgraded ... (schema N -> 4)` so you can tell a no-op from a real
  migration. Upgrading the phux binary does **not** rewrite an already
  installed shim. Schema 1 declares state and stands the detector down;
  schema 2 rewrites the record on every hook and can publish a false
  departure; schema 3 writes identity once; schema 4 adds detector-ingress
  lifecycle reports. `phux doctor`
  reports a stale installed schema and names `phux agent install-claude` as
  the repair.
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
- **`phux new [-s NAME] [-c CWD] [-- COMMAND...] [--json]
  [-e KEY=VALUE]... [--socket P]`** — create a new session. Without `--json`
  it creates and attaches: an explicit `-s NAME` that already exists is an
  error (like tmux's duplicate-session refusal); an omitted name starts from
  `defaults.session-name-template` and gains a numeric suffix when needed; a
  server is auto-spawned if none is running. With `--json` it creates the
  session without attaching (no attach, no resize), then prints the seed pane
  id as JSON and exits. `--json` requires an explicit `-s NAME` — enforced by
  the parser itself, so omitting `-s` is a usage error (exit `2`) — and errors
  if that name is already in use (create-only, never create-or-attach). Repeat
  `--env KEY=VALUE` to add seed-process environment entries; `--env` requires
  `--json`. Shape in §4.4.
- **`phux launch INTEGRATION [--list|--print] [--target TARGET
  [--split horizontal|vertical] [--ratio R]] [-c CWD] [--json] [--socket P]
  [-- ARGS...]`** — resolve an enabled plugin integration and spawn it through
  its identity wrapper. A template may declare bounded native fresh/resume argv
  through a dedicated `PHUX_*_SESSION_ID` environment name; the identity is one
  opaque, non-option argv element and cannot become executable or evaluator
  source. Fixed plugin-owned interpreter wrappers remain valid. Launch
  atomically publishes and confirms the exact Terminal-scoped resume record
  before succeeding. `--list` inventories integrations;
  `--print`/`--dry-run` resolves the same final argv without a server;
  `--target` places the launched pane beside an exact local pane. Successful
  `--json` launch shape is in §4.13.
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
- **`phux insert-pane TARGET NEW_PANE [--split horizontal|vertical]
  [--ratio R] [--json] [--socket P]`** — insert an already-created local pane
  beside an existing layout leaf. This never spawns: create `NEW_PANE`
  separately first. Both selectors must each match exactly one pane in the
  same session. `--split` is the same axis flag `spawn` and `launch` take
  (`h` / `v` shorthands accepted): `vertical` means a vertical divider
  (side-by-side panes); `horizontal` means a horizontal divider (stacked
  panes) and is the default. The pre-unification boolean `--horizontal` /
  `--vertical` spellings have been removed. Shape in §4.12.
- **`phux move-pane SOURCE TARGET [--split horizontal|vertical] [--ratio R]
  [--json] [--socket P]`** — collapse `SOURCE` out of its old position and
  insert it beside `TARGET`. When the panes belong to different sessions, the
  live Terminal is re-parented without restarting its process or changing its
  id, then both sessions' layout envelopes are updated. Shape in §4.12.
- **`phux swap-pane FIRST SECOND [--json] [--socket P]`** — exchange two leaf
  positions without changing split geometry. Shape in §4.12. All three spatial
  verbs reject multi-match and satellite selectors; `insert-pane` and
  `swap-pane` also reject cross-session selectors. None changes an attached
  client's local focus.
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
  phux workspace as a typed JSON archive. Native agent sessions established by
  `phux launch` are copied from their exact Terminal into archive schema 2.
  With no `--output`, the archive is printed to stdout. This contacts the
  server but does not attach or resize.
- **`phux workspace restore ARCHIVE [--socket P]`** — recreate sessions missing
  from a saved archive. A saved native agent identity is resumed only after the
  current enabled integration resolves to the same owning plugin. Restore
  starts new processes; it does not claim to resurrect the original PTYs.
- **`phux worktree new BRANCH [--repo PATH] [--session NAME] [--json]`** —
  create the git worktree and its bound phux session. `--json` returns the
  facts from that create without a follow-up lookup:
  `{schema_version, branch, path, session, terminal_id}`. `terminal_id` is the
  seed pane's numeric id. The sibling `open` and `remove` verbs also accept
  `--json`; all three use the shared `workspace` error code for git failures.
- **`phux --skill`** — print the agent skill compiled into this exact binary
  (`phux skill` is equivalent). Add `=quick`, `=agent`, `=terminal`, or
  `=full`; bare output is `full`. Every scope is derived from one source.
  Prefer it to a copied checkout example when teaching another agent: CI
  checks that it names every visible top-level and `agent` verb.
- **`phux --capabilities --json`** — socketless installed-build discovery:
  phux and wire versions, every visible command path from the live parser,
  skill scopes, CLI JSON contract versions (including intentional unversioned
  results and streams), and actual sibling/PATH discovery of the `phux-mcp`
  companion. It reports compile-time capability, not negotiated running-server
  state; use
  `status --json` for that. Its MCP launcher is `phux mcp`, and
  `phux mcp --schema` prints the authoritative tool input catalog.
- **`phux doctor [--json]`** — inspect the installation, including the
  on-disk Claude shim schema. A stale shim is a warning, not a failed doctor
  run, and the remedy is to rerun `phux agent install-claude`.
- **`phux host <add|ls|rm> [--role remote|satellite] [--json]`** — manage
  both machine registries through one namespace (`--role remote`, the
  default, edits `[[remote]]`; `--role satellite` edits `[[satellites]]`).
  These never contact a running server and never open a transport; they only
  edit local config. `--json` emits the host registry document (§4.10);
  failure paths leave stdout empty and report one contract line on stderr.
  Formerly the separate `phux remote` and `phux satellite` verbs, absorbed
  into this one namespace (ADR-0066).

`insert-pane` is intentionally not named `split`: it edits topology around a
pane that already exists and performs no implicit spawn. Spawn-and-place remains
a separate operation. Self-`detach` (`C-a d`, `FrameKind::Detach`) is still an
interactive TUI-only action — it ends the calling client's own attachment, and
a headless caller was never attached to end. Forcibly detaching *other*
clients is a different, request/response operation
(`Command::DetachClients`, backing `phux detach [SESSION]`); it has no CLI
`--json` today, but is reachable headlessly via the MCP `phux_detach` tool
([`mcp.md`](./mcp.md) §3.8), which talks to it directly over the wire rather
than shelling out. The shipped verbs are listed in [`tui.md`](./tui.md) §1.

**Destructive boundary.** An agent must resolve and display the exact target,
snapshot relevant state, explain what will be lost, and obtain affirmative human
confirmation before `kill` or a destructive signal. The MCP signal adapter also
requires `confirm: true` for interrupt/terminate/kill, and `phux_detach`
requires it too — not because it destroys data (the session and its panes
keep running), but because it forcibly ejects whatever human or agent is
currently attached without their say-so. A watcher ending is not
proof of completion; verify inventory or terminal state under a finite bound.

**How `new` decomposes on the wire.** Session create is no longer an L1
session verb. Per
[ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md) §5,
the session lifecycle verbs were removed from L1 and decompose into substrate
primitives plus L3 metadata: `new` is `SPAWN_TERMINAL` plus an L3 metadata
write on the `phux.session.create/v1` key (the assigned identity is read back
via a nonce-correlated `phux.session.created/v1/<request_token>` one-shot
result), and rename is an L3 metadata SET on the
`phux.session.name/v1` key. Grouping conventions are owned by
[`../spec/L3.md`](../spec/L3.md). The user-facing UX of `new` is unchanged; the
divergence is on the wire, where the migration to this decomposition is
tracked against ADR-0030. `GroupId`'s retention as an opaque grouping key is
settled, not a remnant awaiting removal (bead phux-0bmc closed as
resolved-by-rename).

The alternate-screen transcript harvest in
[ADR-0078](../../ADR/0078-alternate-screen-history.md) remains proposed and is
not a usable read surface. `snapshot --tail` continues to expose only retained
main-screen history.

**Socket precedence (once, for every verb).** The `--socket` argument wins,
then the `PHUX_SOCKET` environment variable, then the daemon default:
`$XDG_RUNTIME_DIR/phux/phux.sock`, falling back to `/tmp/phux-$UID/phux.sock`.

## 3. Targeting: the selector grammar

One grammar, every targeted command — `kill`, `snapshot`, `wait`, `watch`,
`send-keys`, `paste`, `run`, `ask`, `resize`, `agent wait`,
`agent send-keys`, `agent prompt`, `agent answer`, launch/spawn placement, and the
three spatial verbs all share `TARGET`.
It is resolved client-side against a server snapshot ([ADR-0021](../../ADR/0021-control-plane-commands.md)); the server
never parses a selector.

The full grammar table and CLI examples live in [`tui.md`](./tui.md) §3. In one
line, the forms are: `.` (current), `name` (session), `name:N` / `name:tag`
(window), `name:N.M` (pane), and `@N` (opaque id). `=` is explicitly
unsupported for headless commands because they have no attached-client MRU.

`%name` is reserved for the proposed agent-name addressing contract in
[ADR-0075](../../ADR/0075-agent-name-addressing.md), but no shipped CLI verb
resolves it yet. It fails closed as a selector miss rather than choosing a
pane. Use the direct `@N` returned by inventory and creation verbs until that
ADR is accepted and implemented.

A selector that names several panes (a whole session or window) narrows to a
single pane: the focused pane when it is among the matches, else the first in
snapshot order (the `pick_target_pane` tiebreak the MCP tools share).
Optionality differs per verb: `snapshot`, `wait`, `watch`, and `agent wait`
may omit a target (`agent wait` matching `agent show`/`explain`);
`send-keys`, `paste`, `run`, `ask`, `resize`, `agent send-keys`, `agent prompt`,
`agent answer`, and every
spatial verb require it. `launch`/`spawn`
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
`unreachable`. `attached_clients` arrived later *without* a bump — adding a
key is non-breaking under this contract (consumers ignore unknown keys), so
`schema_version` moves only when a key is removed, renamed, or retyped.
Shape, name-sorted:

```json
{
  "schema_version": 3,
  "sessions": [
    { "name": "work", "windows": 3, "attached": true, "attached_clients": 2 }
  ],
  "terminals": ["@3", "devbox/@7"],
  "unreachable": []
}
```

`windows` is the window count; `attached_clients` is the number of currently
attached clients; `attached` is the same fact as a bool
(`attached_clients > 0`), kept for consumers that branch on it. A payload
from a pre-`attached_clients` `phux` simply lacks the key. `terminals` is the
complete addressable inventory in snapshot order, using the canonical direct
selector syntax. Satellite entries intentionally do not imply a hub-local
session/window join.

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

The MCP `phux_ls` tool ([`mcp.md`](./mcp.md) §3.1) executes and parses this
same canonical CLI document, so one parser covers both surfaces.

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
| `soft_wrap` | `Option<{lines: [u32], scrollback: [u32]}>` | Indices of returned rows that continue onto the row below, from libghostty's per-row wrap bit. |
| `truncated` | `bool` | True when the requested window dropped older rows. Absent means `false`. |
| `truncated_reason` | `Option<String>` | Why. The only value this server produces is `"row_window"`. |
| `title` | `Option<String>` | The pane's OSC 0/2 title, when it set one. |

**`soft_wrap` is a three-way answer, not a two-way one.** Present and
non-empty is "these rows wrap"; **present and empty** is "wrapping was
reported, and nothing wraps"; **absent** is "the producer says nothing about
wrapping," which today identifies a server predating the field. A consumer
that unwraps by default has to tell the last two apart, and no version number
can express the difference — the older server would not have moved one either.
Indices are per-array, and a wrapped final `scrollback` index continues into
`lines[0]`: history and viewport are one stream for wrapping purposes, so
joining them per-array is wrong. `phux wait` already unwraps (§2); a consumer
doing its own substring matching should too, because a match against raw
`lines` silently fails when the text straddles a wrap.

**`truncated` is scoped to the requested window** — `--scrollback N` against
more retained history than N, or a `--tail` window narrower than the rendered
stream. It says nothing about rows the emulator itself evicted from its history
ring long ago, which is unknowable. `truncated_reason` is a string rather than
an enum precisely so a future reason is not a hard deserialize failure;
tolerate a value you do not recognize.

**`title` is the ADR-0046 detector's highest-ranked evidence** and no other
read surface exposed it, so an offline `agent explain --file` capture lost it.
`None` means the pane set no title *or* the producer predates the field; both
are "no title to reason about," which is the same fail-safe answer.

**`schema_version` did not move for any of these four, and that is the
contract working, not an oversight.** §4.1's rule governs the whole family: a
version moves when a key is **removed, renamed, or retyped**, never when one is
added, because consumers ignore keys they do not know. All four are
`#[serde(default)]` with `skip_serializing_if`, so an untruncated, title-less
snapshot with no wrap data serializes byte-identically to the pre-ADR-0077
shape. A consumer probing for wrap support therefore tests for the *presence*
of `soft_wrap`, not for a version — the version is what the server can do, not
what this payload contains.

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
ignores extra keys. `schema_version` is the signal for a *breaking* change —
a removal, rename, or retype — not for an added key; probe for the key itself
when you need to know whether a producer supplies one.

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
{ "schema_version": 1, "session": "NAME", "terminal_id": 2 }
```

It is create-only: `--json` requires an explicit `-s NAME` (a parse-time rule;
omitting `-s` is a usage error, exit `2`) and errors (exit `1`)
if that name is already in use. Repeat `--env KEY=VALUE` to inject environment
entries into the seed process; values may contain additional `=` characters.
The wire decomposition behind it is in §2.

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

#### `AgentWaitJson` — `phux agent wait --json`

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

`edge` is `null` exactly when `satisfied` is `false` — the timeout case, exit
124, where the document still goes to **stdout**, matching `phux wait --json`.
Only the typed failures of §5.3 leave stdout empty. `via` is `"push"` (a server
change notification) or `"poll"` (the re-read floor recovering an edge the push
half never delivered); it is diagnostic, not contract-critical.

`baseline` is the pre-wait level: **recorded and never evaluated**. It is in
the document so a caller that timed out can see it was already resting in the
state it asked for — the one outcome the edge rule makes surprising.

`detection` is one entry of the `agents` array above — the same object, with
`confidence` and the full `sources` evidence trail for this pane — so a caller
can tell a state an integration *declared* (`agent_record` provenance) from
one a screen rule derived. On a pane phux itself instrumented that trail is
always the derivation: the shipped hook shim declares identity only, so no
state on such a pane comes from a hook. It is `null`, as shown, when the post-wait
read fails; that degrades the document by one optional field and never fails a
wait that was already satisfied. Without `--json` the prose line is
`@N<TAB>name<TAB>from -> to<TAB>via push|poll<TAB>confidence`.

#### `phux agent send-keys --json`

```json
{
  "schema_version": 1,
  "terminal": "@7",
  "agent": { "name": "reviewer", "kind": "claude" },
  "keys": 2,
  "verified": true,
  "delivery": "ok",
  "operation_id": "...",
  "attempts": 1
}
```

Emitted only on a fully delivered batch; without `--json` the verb says
nothing on success, exactly as `phux send-keys` does. `keys` is the number of
key specs delivered, and `verified` records that the occupant check ran and
passed. Refusals follow §5.3.

#### `phux agent prompt --json`

The result records both halves of the fused operation. On wait timeout it
still goes to stdout and the process exits `124`; `delivery: "ok"` means the
prompt reached the kernel tty queue even though no target transition was
observed.

```json
{
  "schema_version": 1,
  "terminal": "@7",
  "delivery": "ok",
  "operation_id": "...",
  "agent": { "name": "reviewer", "kind": "claude", "state": "working", "session": null },
  "pre_submit_state": "idle",
  "staleness_bound_ms": null,
  "attempts": 1,
  "submit_ms": 8,
  "transition_observed": true,
  "matched_by": "transition",
  "edge": { "from": "working", "to": "idle", "via": "push" },
  "waited_ms": 1200,
  "degraded_to_polling": false
}
```

#### `phux agent answer --json`

```json
{
  "schema_version": 1,
  "terminal": "@7",
  "ask": { "id": "deploy", "question": "Deploy?", "suggestions": ["Yes", "No"] },
  "answer": "Yes",
  "source": "choice",
  "operation_id": "...",
  "delivered": true
}
```

#### `phux agent start --json`

The ready result includes `terminal`, `name`, `kind`, `integration`, `started`,
`ready`, `state`, `shell_check`, and a `readiness` object containing identity,
transition, detector provenance, latency, and observation counts. With
`--no-wait`, `ready` is `false` and `readiness` is `null`. A readiness timeout
is a §5.3 error document on stderr and exits `124`; the launch command was
already delivered.

#### `AgentExplainJson` — `phux agent explain --file ... --json`

The offline explainer emits a different document from `AgentStateJson`: it
reports what the detection manifests did to one captured screen, not what phux
believes about a live pane. The top-level key is `explain`, so the two are
distinguishable without reading further.

```json
{
  "schema_version": 1,
  "capture": {
    "path": "screen.json",
    "format": "json",
    "rows": 42,
    "cols": 120,
    "title": "phux"
  },
  "explain": {
    "kind": "claude",
    "name": "claude",
    "state": "blocked",
    "detector_state": "blocked",
    "matched_rule": "prompt-permission-dialog",
    "freeze": false,
    "visible_idle": false,
    "regions": [
      { "region": "title", "empty": false, "lines": ["phux"] },
      { "region": "prompt-box", "empty": true, "lines": [] }
    ],
    "evaluated_rules": [
      {
        "id": "prompt-permission-dialog",
        "priority": 80,
        "region": "after-last-rule",
        "state": "blocked",
        "matched": true,
        "visible_idle": false,
        "skip_state_update": false,
        "evidence": {
          "op": "all",
          "matched": true,
          "children": [
            { "op": "contains", "pattern": "do you want to ", "matched": true },
            { "op": "line-regex", "pattern": "^\\s*\\d+\\.\\s+\\S", "matched": true }
          ]
        }
      }
    ]
  }
}
```

`capture.format` is `json` or `text` — what the bytes were actually parsed as,
after `--format auto` sniffed them. `capture.cols` is `null` for a text
capture, which declares no grid width. `capture.title` is the `--title` value,
or the empty string when none was supplied.

`state` is the state a rule asserted and is absent when none did.
`detector_state` is what the detector would publish: the asserted state, else
`idle` when nothing matched, else `frozen` when a `skip-state-update` rule
matched and the previous state is held. When the two differ, `fallback_reason`
says which case applied. `detector_state` is the field to branch on.

`regions` covers every region the manifest grammar offers — `title`,
`prompt-box`, `after-last-rule`, `bottom-lines`, `viewport`, in that order —
whether or not a rule names it, and `empty` is `true` when the region resolved
to nothing a predicate can see. Region previews are never elided in JSON; the
prose form caps each region and reports how many rows it dropped.
`evaluated_rules` lists every rule in declaration order including the misses;
`evidence` is the predicate tree with a per-node `matched`, and every child of
a combinator is evaluated, so a failing `all` shows which conjunct failed
rather than only that one did. `region` and `evidence[].op` use the manifest's
own spellings, so a value read here can be typed straight back into a TOML
rule.

Failures follow §5.3, with the codes named there.

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
  "schema_version": 2,
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
              "agent_session": {
                "plugin_id": "com.phux.agent-tools",
                "integration_id": "codex",
                "native_id": "019c2f31-77d2-7a93-8931-47d27b46ceda"
              },
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
starts the default shell in the saved cwd when available. `agent_session` is
also nullable. When present, it is inert provenance, not executable input:
restore re-resolves the current integration, requires the same `plugin_id`, and
builds structured resume argv from the current template. It never replays
archived argv as resume authority. Existing session names are skipped, and
restore prints a schema-2 summary JSON document with `restored` and
`skipped_existing` arrays. Schema-1 archives remain readable and retain their
fresh-process behavior.

Restored sessions are fresh PTYs. The archive preserves window/pane metadata and
split-layout shape for inspection and future replay, but the current restore
command only recreates missing sessions and their seed process. Use `phux
upgrade` for live PTY handoff across a server re-exec; do not present workspace
restore as resurrecting already-running processes.

### 4.10 Host registry — `phux host ... --json`

The machine-registry surface is config-local (`host enroll` excepted — it
drives ssh). `add`, `ls`, and `rm` edit or read `[[remote]]` and
`[[satellites]]` entries and do not dial remote hosts.

`phux host ls --json` emits one merged document:

```json
{
  "schema_version": 1,
  "hosts": [
    {
      "name": "devbox",
      "role": "satellite",
      "endpoint": "ssh://devbox",
      "enabled": true,
      "token_file": null,
      "cert_fingerprint": null,
      "session": null
    }
  ]
}
```

`enabled` is `null` for `role: "remote"` entries (the schema has no enabled
bit) and `session` is `null` for satellites (a hub-dialed link has no
arrival to attach). `--role` filters the array to one registry. `add --json`
and `enroll --json` wrap one such object under `"host"`; `rm --json` emits
`{"schema_version": 1, "removed": {"name": ..., "role": ...}}`. Invalid
names, invalid endpoint URIs, duplicate configured names, and refused
registry writes are hard failures: exit nonzero, stdout empty, one contract
line (§5.3) on stderr.

### 4.11 `phux spawn --json`

`phux spawn --json` emits a small fixed object naming the spawned terminal:

```json
{
  "schema_version": 1,
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
`source_terminal_id`, `target_terminal_id`, `direction`, and `ratio`. A
cross-session move also adds `source_session_id` and `cross_session: true`,
while `session_id` names the destination. Swap adds `first_terminal_id` and
`second_terminal_id`. `direction` retains the CLI's user-facing divider meaning
(`vertical` = side-by-side, `horizontal` = stacked), not the layout tree's
internal child-axis enum.

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

With `--json`, failures emit the shared JSON error contract (§5.3) on stderr
and leave stdout empty. Stable codes for spatial edits include
`invalid_selector`, `selector_miss`, `selector_not_single`,
`satellite_target`, `cross_session`, `same_pane`, `invalid_ratio`,
`layout_missing`, `pane_not_in_layout`, `pane_already_in_layout`, and
`layout_rejected`. Cross-session moves may also report `server_too_old`,
`move_refused`, `post_move_state_failed`, `destination_changed`,
`destination_layout_failed`, or `source_layout_failed`; these are exit `1`
because ownership or transport work has begun, while preflight selector and
layout refusals remain exit `2`.

### 4.13 `phux launch --json`

A successful launch returns the resolved integration identity, the final argv
actually spawned (including a generated fresh identity or explicit resume
identity when its template declares one), and the new local terminal id:

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
resolved `cwd`, `working_directory`, and the same prepared `argv` without
spawning. Placement does not add a second result shape: `--target`, `--split`,
and `--ratio` affect the persisted topology while the launch JSON remains the
object above.

### 4.14 `phux rec --json`

One object on stdout on success, and nothing else on stdout ever. Progress and
every diagnostic go to stderr, and progress is suppressed entirely under
`--json`:

```json
{
  "schema_version": 1,
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

Unlike §4.1–§4.3's engine-state projections, this object has no producing
struct in `phux-core` — it is a plain result line — but it carries the same
`schema_version` contract as every other `--json` verb in this catalog.

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

### 4.16 `phux play --json`

One object on stdout naming the pane that was created and the recording it is
playing:

```json
{
  "schema_version": 1,
  "terminal_id": 7,
  "path": "/home/me/demo.cast",
  "cols": 80,
  "rows": 24,
  "events": 63,
  "speed": 1.0,
  "idle_limit": 2.0,
  "duration_ms": 17198,
  "passes": 1
}
```

`terminal_id` is the payload: everything you do next — `snapshot @7`,
`resize @7`, `rec @7`, `kill @7` — is addressed by it. `path` is absolute,
because the pane's process resolves it from the daemon's working directory and
not yours. `cols`/`rows` are the *recording's* grid, which is also the size
the pane is fitted to unless `--no-fit` was given or something else owns the
pane's size. `duration_ms` is how long the playback will take at the requested
speed, after the idle clamp — the wait you are actually in for, not the
recording's raw length. `idle_limit` is the clamp that was applied (`null`
when none was). `passes` is the number of times the recording will play, and
`null` means it repeats until the pane is killed.

Like §4.14, this object has no producing struct in `phux-core` — it is a
result line, not a projection of engine state — but it carries the same
`schema_version` contract as every other `--json` verb in this catalog.

Exit codes: `0` once the pane exists; `1` on failure (no server, unreadable or
malformed cast, unresolvable TARGET, a refused spawn). A failure creates no
pane: the cast is parsed in the caller's own process, before anything is
spawned.

### 4.17 `phux tag --json`

All three tag actions (`ls`, `add`, `rm` — and their `list` / `remove`
aliases) emit one `schema_version: 1` document with a row per resolved
Terminal:

```json
{
  "schema_version": 1,
  "terminals": [
    { "terminal": "@7", "tags": ["build", "ci"] },
    { "terminal": "edge/@3", "tags": [] }
  ]
}
```

`terminal` is the canonical, reusable selector for that Terminal — `@N`
locally, `host/@N` for a satellite pane through a hub — so each row's id can
be fed straight back into any TARGET-taking verb. `tags` is the Terminal's
complete tag list: for `ls` as stored, and for `add` / `rm` as **read back
from the server after the write** (the confirming `GET_METADATA`
round-trip), never echoed from the request. An untagged Terminal is an empty
list, not an absent key.

Failures follow §5.3: a dead socket emits the contract with `no_server`, an
unparseable TARGET `invalid_selector`, and a selector miss splits
`no_such_target` (exit 1) from `partial_view` (exit 3) exactly as the prose
path splits the exit codes (§5.2). Partial-fleet warnings on a *successful*
resolution stay prose on stderr ahead of the document.

### 4.18 `phux pair --json`

`phux pair --json` never contacts a running server (see [`remote-access.md`
§"Pairing"](../remote-access.md)); it mints or reads a bearer token and
reports it alongside everything a device needs to dial this host:

```json
{
  "schema_version": 1,
  "credential_id": "0123456789abcdef0123456789abcdef",
  "generation": 1,
  "token": "deadbeef...64 hex chars",
  "cert_fingerprint": "AB:CD:...64 hex chars",
  "overlay_addresses": ["100.64.0.2"],
  "ws_addr": "0.0.0.0:8787",
  "quic_addr": null,
  "connect_link": "phux://connect?url=wss://100.64.0.2:8787&token=deadbeef...",
  "tokens_path": "/home/me/.local/state/phux/remote-tokens"
}
```

`ws_addr` and `quic_addr` are the server's *configured bind* (from the
environment its listener reads), not a resolved dialable address — pair them
with an `overlay_addresses` entry to build one, which is exactly what
`connect_link` already did for you. Each is `null` when this host has no
listener of that kind configured; `phux host enroll` reads that as the
signal to fall back to `ssh://`. `overlay_addresses` is best-effort
(ADR-0037) and empty, never absent, when nothing was detected.
`connect_link` is `null` whenever no address source (neither `--host` nor a
detected overlay address plus a known port) exists to build one from — a
device then has to be given the address by another channel. The token
printed in this document is a secret and is only ever emitted once; it is
not re-derivable from the token store afterwards. `credential_id` is the
non-secret stable ID used with `phux pair rotate` and `phux pair revoke`;
`generation` starts at one and increments on rotation.

`phux pair rotate CREDENTIAL_ID --json` emits a separate schema-version 1
operation document with `operation: "rotate"`, `credential_id`, the new
`generation`, one-time `token`, `overlap_seconds`, and `tokens_path`. `phux
pair revoke CREDENTIAL_ID --json` emits `operation: "revoke"`,
`credential_id`, and `tokens_path`; it never emits any bearer token. Rotation
keeps prior generations valid only for the requested overlap and never beyond
their existing absolute expiry. An already-expired credential is rejected
before a replacement token is generated, so failed rotation emits no JSON
document or secret. Revocation and rotation affect new connections; an
established session retains its admission until it disconnects.

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

**Supervising another agent is the same loop with a different wait.** A pane
running an agent has a lifecycle record, not a sentinel, so wait on the record:

```sh
phux agent prompt --expect-agent reviewer --wait \
  --until idle --until blocked --timeout 900 --json @7 "review the diff"
phux snapshot --json --tail 200 --unwrap @7 > transcript.json
```

Read `agent prompt`'s answer, do not assume it. Exit `0` means a transition into
one of those states was **observed**; `124` means none was, which is *not* the
same statement as "the agent is still working" — check `phux agent show` for
the level and `edge`/`baseline` in the `--json` document for what the wait
actually saw. Exit `1` is a departure: the record went away mid-wait, or its
`state` was withdrawn to `unknown` because the pane's occupant died or
changed. Neither must ever be read as completion. Why the verb refuses to
answer from the current level at all is in §2 and, normatively, in
[`../spec/L3.md`](../spec/L3.md) §3.7.

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
| `agent` | `0` ok; `1` no server, unknown pane, or JSON render failure; `3` the miss is not trustworthy — see below (`show`/`explain`/`set`/`clear`; `list` enumerates and stays `0`; `wait` and `send-keys` have their own rows and keep `1`). |
| `run` | the child's own code clamped to `0..=255` (negative or `>255` saturate to `255`); `125` when phux gave up waiting for the sentinel (`--timeout`); `1` for no server / refused target / other. |
| `wait` | `0` condition met; `124` on `--timeout`; `2` usage — an invalid `--regex`, or `--until` combined with `--regex` (clap's own status, raised before any poll); `1` no server / parse / read error. |
| `agent wait` | `0` a transition into a `--until` state was observed; `124` on `--timeout`, including a pane that held a target state for the whole wait; `2` the pane declares no record, an unknown `--until` word, or a satellite target (`satellite_target` — `phux.agent/v1` does not federate); `1` the agent departed mid-wait (record deleted or state withdrawn to `unknown`), no server, or transport. |
| `agent send-keys` | `0` the acknowledged batch reached the kernel tty queue; `2` a refusal before any byte was written; `1` transport, selector, or indeterminate delivery. Do not resend `delivery_unknown`. |
| `agent prompt` | `0` delivered (and, with `--wait`, a target transition observed); `124` delivered but no target transition observed before timeout; `2` usage, identity, capability, or pre-write refusal; `1` transport or indeterminate delivery. |
| `agent answer` | `0` the exact live ask was validated and the answer delivered; `2` stale/unidentified ask, invalid choice/text, or pre-write refusal; `1` transport or indeterminate delivery. |
| `agent start` | `0` submitted, and ready unless `--no-wait`; `124` command typed but readiness not observed; `2` invalid name/kind/argv, no manifest, or unsafe target; `1` launch, transport, or observation failure. |
| `new` | `0` ok; `1` duplicate `-s` name / failure. |
| `resize` | `0` the pane holds the requested geometry; `1` no server / selector miss / unknown pane, or the server holds a different size (an attached view's `window-size` policy owns it); `2` unusable `COLSxROWS` (clap usage error, raised before any connection). |
| `rename` | `0` renamed; `1` no server or transport failure; `2` unknown source session or destination name already exists. |
| `launch` / `spawn` | `0` spawned/resolved/listed; `1` invalid integration, placement, server, or spawn failure. |
| `watch` | `0` Ctrl-C, plain EOF, or an `--until` match; `124` `--timeout`; `2` unknown event name; `1` transport or EOF before a requested event. |
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
`wait`, `watch`, `resize`, `signal`, `rec`, `ask`, `agent wait`, and
`agent send-keys`, `agent prompt`, and `agent answer` share one resolver, and
some of them have already spoken for the number: `run` mirrors the child's own
exit code (a command may legitimately exit `3`), `wait` and `agent wait` own
`124`, and `agent wait` / `agent send-keys` spend `2` on their own refusals.
Those verbs keep `1` and say it in words. Branch on the status where the table below
offers `3`; otherwise read stderr, which never claims absence it cannot
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

### 5.3 The JSON error contract

Every core server-talking verb above
(`ls` / `snapshot` / `wait` / `run` / `watch` / `resize` / `spawn` / `launch` /
`play` / `rec` / `new` / `ask`, plus the spatial edits of §4.12), and every
`--json`-bearing registry and inspection verb (`tag`, `plugin`,
`remote list`, `satellite`, `worktree list`, `workspace inspect`,
`config check`, `logs`, `doctor`, `agent explain --file`, `agent wait`,
`agent send-keys`, `agent prompt`, `agent answer`, `agent start`), reports a
`--json` failure the same way:
**stdout stays empty** (the document channel never carries half a result)
and **stderr carries one line of JSON** (ADR-0065 §4):

```json
{
  "schema_version": 1,
  "error": { "code": "no_server", "message": "no server running at /run/phux.sock" },
  "remedy": "start one with `phux` (attaches, auto-starting a server) or `phux server`; ...",
  "exit_code": 1
}
```

- `schema_version` is `1`; new fields are additive and do not bump it.
- `error.code` is a **closed vocabulary** owned in one place by
  `commands/json_err.rs`; branch on it, never on `message` text. The
  transport family: `no_server` (nothing listening at the socket),
  `server_disconnected` (the server went away mid-command), `transport`
  (any other transport/protocol failure). The resolution family:
  `no_such_target` (a miss against a complete view) and `partial_view` (a
  miss against an incomplete fleet — the target may exist on an unreachable
  satellite; retry, per §5.2's exit-3 discussion). Spatial edits add the
  codes listed in §4.12. The registry family: `registry` (a local
  `[[plugins]]` / `[[remote]]` / `[[satellites]]` config-registry read,
  validate, or write failed), `workspace` (a git workspace/worktree
  operation failed — not a repository, git failed, or its output did not
  parse), `invalid_config` (`config check` could not run at all —
  unreadable file or malformed TOML; exit 2, mirroring its prose path's
  distinct "could not check" status), and `json_serialize` (a result
  document failed to render — a phux bug worth filing). The update family
  (`phux update`): `update_invalid_tag` and `update_unsupported_platform`
  (exit 2 — a tag that is not `vX.Y.Z`, or a platform with no published
  artifact); `update_source_unsupported`, `update_immutable_store`, and
  `update_package_managed` (exit 2 — phux will not write to this install, and
  `remedy` carries the exact native command); `update_fetch_failed`,
  `update_checksum_invalid`, `update_checksum_mismatch`,
  `update_archive_rejected`, `update_install_failed`, and `update_no_backup`
  (exit 1). A `update_checksum_mismatch` means the published digest and the
  downloaded bytes disagreed: nothing was unpacked and nothing was installed.
  The offline-explain family (`phux agent explain --file`, which talks to no
  server and so reaches none of the transport codes): `capture_unreadable`
  (the file or stdin could not be read) and `capture_invalid` (the bytes are
  not a screen — JSON that is not a `ScreenState`, or a capture with no rows),
  both exit 1; and `unknown_agent_kind` (exit 2 — `--kind` was omitted or
  names no loaded detection manifest, with the roster in `remedy`). The
  agent-lifecycle family (`agent wait`, `agent send-keys`, `agent prompt`,
  `agent answer`, `agent start`):
  `no_agent_record` (exit 2 — the pane declares no `phux.agent/v1` record, so
  there is no lifecycle to wait on and no identity to verify against),
  `satellite_target` (exit 2 — the pane belongs to a federation satellite;
  `phux.agent/v1` is hub-local and does not cross a satellite link, so a hub
  can neither observe nor write it. Refused as soon as the selector
  resolves, so nothing was read and nothing was typed. Run the verb against the
  satellite's own server. This is *not* `no_agent_record`: the remote pane
  may well have a live agent),
  `agent_departed` (exit 1 — the record was deleted or its state withdrew to
  `unknown` mid-wait; a departure, never a completion), `agent_mismatch`
  (exit 2 — the pane hosts a different agent than `--expect-agent` /
  `--expect-kind` named, and nothing was written), and `invalid_key_spec`
  (exit 2 — a key argument would not translate to the key you clearly meant,
  refused before the connection is opened so the batch stays all-or-nothing).
  Acknowledged input adds `input_busy` (nothing written; retry is safe),
  `input_not_written` (exit 1 — nothing was written, **proven** at some point
  other than lane contention: no PTY, a writer-side queue full or closed, or
  the pane's own actor gone before handoff; retry is safe, under the same
  operation id or a fresh one, because nothing already written could be
  duplicated), `delivery_unknown` (exit 1 — indeterminate; never resend
  under any id — do not confuse the two: `input_not_written` is the case
  the server can rule out delivery for, `delivery_unknown` is the case it
  cannot), `input_too_large`, `input_lease_held`, `canonical_limit_exceeded`,
  `unsafe_paste`, `invalid_input_batch`, and `permission_denied`. Ask validation adds
  `no_active_ask`, `ask_unidentified`, `ask_stale`, `answer_choice_out_of_range`,
  and `answer_not_suggested`. Start adds `invalid_agent_name`,
  `unsupported_agent_kind`, `agent_detection_unavailable`,
  `agent_name_conflict`, `target_not_shell`, `invalid_launch_argv`,
  `ambiguous_integration` (exit 2 — more than one enabled integration's
  `[agent_identity]` claims the requested `--kind`; the message names every
  claimant and `--integration ID` chooses), `agent_start_timeout`, and
  `agent_kind_mismatch`. Watch adds `unknown_event_name`.
- `remedy` is always present and non-empty: the next command to run, in
  prose.
- `exit_code` mirrors the process's own exit status, so a consumer that
  only captured stderr still learns it. Exit-code semantics are unchanged
  from §5.2 (`0` ok, `1` miss / no server, `2` refusal / usage, `3` partial
  view where the verb can spend it, `124`/`125` timeouts) — under `--json`
  a partial-view miss carries `partial_view` in `error.code` even for the
  shared-resolver verbs whose status must stay `1`.
- Warnings (e.g. partial-fleet notices on a *successful* resolution) still
  precede the error line on stderr as prose; the JSON error object is the
  final line.

Without `--json` the same failures stay prose (message plus an indented
remedy block), so nothing changes for humans or for scripts that grep
stderr.

**Why there is no `-j` short flag.** Considered and rejected in ADR-0065 §7:
`--json` is typed almost exclusively by scripts and agents, where
explicitness is worth more than two saved characters, and the binary keeps
its short-flag surface reserved for high-frequency human-typed options.

## 6. Relationship to the other agent surfaces

The CLI verbs here are the stable contract. The
[OpenCode integration](./opencode.md) selects a host-specific six-tool subset;
the [Pi integration](./pi.md) exposes nineteen bounded tools, including spatial
placement and topology edits. The [MCP adapter](./mcp.md) exposes 32 strict
tools, including paste, launch/spawn, bounded watch, ask, spatial edits, agent
state, and workspace parity, over JSON-RPC stdio. Adapter guides link here instead of
redefining CLI syntax.
[`sdk.md`](./sdk.md) documents `phux-client`, the library crate those surfaces
are built from. These adapters are unprivileged consumers
([ADR-0017](../../ADR/0017-tui-not-protocol-privileged.md)); the wire
underneath stays additive and versioned under [`../spec/`](../spec/)
([ADR-0022](../../ADR/0022-tool-for-agents.md)).
