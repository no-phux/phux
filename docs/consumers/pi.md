---
audience: humans, agents, consumers, contributors
stability: evolving
last-reviewed: 2026-10-09
---

# Pi integration

**TL;DR.** `@phux/pi` connects Pi to an external local phux server with
twenty-five terminal/control tools and three human commands. It preserves
branch-local targets and appends bounded fleet context without changing Pi's
stable prompt prefix. It writes identity metadata and, on capable servers,
AgentSession events. It neither embeds a terminal nor owns the server.

---

## Requirements and installation

The package requires Node.js 22 or newer, Pi, and `phux` on `PATH`; it does
not bundle phux. Development and packed-extension load gates pin Pi 1.0.4;
host-provided Pi modules remain wildcard peers rather than bundled copies. The installed CLI must support `paste`, `agent prompt/wait`,
`resource wait`, `runtime-info`, and current snapshot/wait options. Older
binaries may support only part of this surface; unsupported verbs fail
rather than being emulated. Check the installed version before loading:

```sh
phux --version
```

Install the independently versioned package with
`pi install npm:@phux/pi`, or `pi install ./integrations/pi` from a trusted
checkout (the repository root is not a Pi package). A trusted checkout of this
repository also loads it through [`.pi/settings.json`](../../.pi/settings.json)
unless a global copy is already configured.

The extension inherits `PHUX_SOCKET`; set it before starting Pi when the server
uses a non-default local Unix socket. It also reads `PHUX_TERMINAL_ID`, which
phux sets for a hosted agent, to identify Pi's own pane in automatic fleet
context and refuse input/destructive tool actions against it. Set
`PHUX_CONTEXT_AWARENESS=0` before startup to disable context, not this protection.
There is no package command for choosing an alternate executable. Library
consumers can construct `PhuxCli` with an absolute `executable`, but the
installed Pi extension expects `phux` on `PATH`.

## First shared-terminal walkthrough

1. Start or locate a local phux server outside Pi. For a first local session,
   `phux new work` creates and attaches interactively.
2. In a separate terminal or pane, run `pi` with the package installed, then
   enter `/phux`. Do not select Pi's own interactive UI as a shell target.
3. Choose the idle shell pane under `work`. The Pi status line shows the saved
   target. Ask Pi to snapshot it without sending input.
4. **Expected:** Pi shows the same shell text you see in phux. Ask it to run
   `pwd` there with `phux_run`; the result should match that shell's directory.
5. Run `/phux-status` before a handoff if the pane may have exited or moved.
6. Run `/phux-attach`. Pi prints an argv such as
   `["phux","attach","work"]` and identifies the pane to navigate to after
   attach. Run the equivalent `phux attach work` in a separate real terminal.
   The extension does not execute it or open a nested terminal inside Pi.

If `/phux` is missing, confirm the package installation and restart Pi. If the
target is **stale** or **unavailable**, inventory again and select a live pane;
do not force a write to an old id. For connection errors, use
[agent connection recovery](../troubleshooting.md#an-agent-or-mcp-host-cannot-see-the-server).
Quit Pi normally to end it. To leave a human phux view while work runs,
press `Ctrl-A`, release both keys, then `d`.

A human attach is not a read-only monitor: agent and human input can
interleave at the PTY, and the package serializes nothing. Coordinate before
typing into a pane the other participant is driving.

### Add and arrange workers

After the first read/write succeeds, ask Pi to create or launch a worker
beside the selected target. For example,
`phux_launch({ integration: "codex", target: "@3", split: "vertical",
ratio: 0.4, alias: "worker" })` creates a side-by-side pane when Codex is
installed and that integration is configured; substitute your actual target.
`split` or `ratio` requires `target`; ratios must be finite and strictly
between 0 and 1. `vertical` means side-by-side and `horizontal` means stacked.

Shape already-created panes with `phux_insert_pane`, `phux_move_pane`, or
`phux_swap_pane`. These mutate persisted topology only: they do not spawn,
focus, take, give, or paste. Insert and move accept optional `direction` and
`ratio`; horizontal is the CLI default. Insert never spawns its `new_pane`.

## Surface

The extension registers exactly these twenty-five model tools:

| Tool | Operation |
|---|---|
| `phux_list` | List phux sessions. |
| `phux_create` | Create a named session without attaching and select its seed pane. |
| `phux_snapshot` | Read a bounded, side-effect-free pane projection; supports `scrollback`, `cells`, `tail`, and `unwrap`. |
| `phux_send_keys` | Send named keys or literal key text to one worker pane; not a paste operation. |
| `phux_paste` | Paste literal multiline text as one input event without appending Enter. |
| `phux_run` | Run one command at a known shell prompt and return its exit result. |
| `phux_wait` | Wait for literal `until`, `regex`, or `idle_ms`; supports `output_only` and `tail`. |
| `phux_agent_prompt` | Submit a single-line agent turn with acknowledged delivery and optional identity checks; wait for a post-delivery lifecycle transition by default. |
| `phux_agent_wait` | Observe agent lifecycle state without sending input. |
| `phux_resource_wait` | Wait for an explicit resource's process exit; accepts an `after` cursor from an earlier wait. |
| `phux_status` | Read canonical server status JSON; distinct from `/phux-status` target status. |
| `phux_runtime_info` | Read runtime discovery/capability JSON. |
| `phux_panes` | Inventory pane ownership, agent state, attention, title, cwd, and evidence. |
| `phux_spawn` | Spawn without attaching; optional local placement, alias, and positive `retain_seconds` for post-exit inspection. |
| `phux_launch` | Launch a configured integration from the CLI's versioned machine result, with optional local placement. |
| `phux_insert_pane` | Insert one already-created exact local pane beside another. |
| `phux_move_pane` | Move one exact local pane beside another, including across sessions. |
| `phux_swap_pane` | Swap two exact local pane leaves without changing geometry. |
| `phux_kill` | With an explicit canonical pane/alias/group and `confirm:true`, destroy the validated targets. Check every group member before any destruction. |
| `phux_signal` | Interrupt, freeze, or resume a canonical worker pane or alias; terminate and kill require an explicit target and `confirm:true`. |
| `phux_tag` | List, add, or remove terminal tags. |
| `phux_ask` | Report a human-attention ask event. |
| `phux_watch_events` | Collect typed events for 50 ms–30 s, then stop the streaming CLI subprocess. |
| `phux_rendered_snapshot` | Capture the attached client's composited frame at bounded dimensions. |
| `phux_targets` | List or mutate branch-local named aliases and groups. |

It registers exactly these three human commands:

| Command | Operation |
|---|---|
| `/phux` | Inventory panes and choose the default target; native picker in TUI, standard `select` dialog over RPC. |
| `/phux-status` | Refresh and report the saved target and its availability. |
| `/phux-attach` | Print a human attach argv; it never executes the attach. |

The [agent CLI guide](./agents.md) owns argument syntax, selector rules,
JSON, and exit codes.

There is no `take` / `give`: a one-shot CLI connection cannot hold an input
lease.

Model-facing output is capped at 200 lines and 12 KiB. CLI stdout and stderr
capture are independently bounded; every subprocess accepts Pi cancellation.
Short calls default to a 10-second local deadline.
`phux_run`, `phux_wait`, `phux_agent_wait`, `phux_resource_wait`, and waiting
`phux_agent_prompt` calls default to `timeout_seconds:30`; the allowed range is
1–86400 seconds. Zero, non-finite values, and indefinite waits are rejected.
Their local subprocess deadline defaults to the chosen operation timeout plus
five seconds. `local_timeout_ms` can deliberately override it (1–86405000 ms),
including a shorter deadline; stopping a local CLI process does not prove that
the remote command or prompt stopped or was never delivered. The watch
adapter requires a finite collection window and returns at most 100 parsed events rather than
leaving an indefinitely streaming subprocess. Results state when the adapter
truncated output and preserve a separate truncation flag reported by phux.

Nonzero shell-command exit codes remain `phux_run` results, not wait failures.
A screen wait timeout returns `outcome:timed_out` with its final screen.
Agent and resource results preserve the CLI's JSON fields, including delivery
receipt, transition/satisfaction status, resource outcome, and cursor where
present. CLI failures retain the runtime's typed error and structured CLI error
record. Cancellation remains cancellation, not a synthetic timeout or missing
inventory. No mutation is automatically retried.

Choose the operation to match the foreground program:

- **Shell:** `phux_run` only at a known shell prompt. Its sentinels are shell
  syntax, not agent instructions.
- **Interactive editor/REPL/TUI:** `phux_paste` preserves literal newlines and
  indentation in one paste event; `phux_send_keys` sends deliberate navigation,
  Enter, or interrupt keys. A newline can still be interpreted as submission
  by a program without bracketed-paste handling.
- **Agent turn:** `phux_agent_prompt` takes single-line text (the CLI enforces a
  4096-byte ceiling), with optional `expect_agent` and `expect_kind` assertions.
  It waits for a post-delivery transition to idle, blocked, or done by default;
  `until` can select lifecycle states. `wait:false` submits without waiting and
  forbids `until` and `timeout_seconds`. An acknowledged receipt proves delivery
  to the input queue, not consumption. A transition wait timeout may follow
  successful delivery. On `delivery_unknown` or local interruption, inspect the
  pane before deciding what to do; never blindly resend.
- **Process completion:** `phux_resource_wait` requires an explicit resource
  identity, not a pane alias/group or implicit selection. A direct `@N` can
  name an already-exited retained resource.

`phux_wait` accepts at most one of `until`, `regex`, and `idle_ms`. `output_only`
filters shell-marked command echo only when OSC-133 integration is present; the
tool preserves the CLI warning when filtering is unavailable. `tail` limits
the logical lines inspected by wait. Snapshot `tail` instead bounds rendered
rows with the viewport as a floor; `unwrap` joins soft-wrapped lines. An idle
screen is not evidence that an agent turn completed.

## Automatic fleet context

Before each new Pi agent run, the extension reads the public agent inventory.
The first observation is a hidden `phux-context` checkpoint appended after the
new user message. Later changes append sequenced deltas; an unchanged inventory
adds no message. The base system prompt, context files, and tool definitions
remain unchanged. Each message tells the model that the latest sequence
supersedes older phux context and that all values are untrusted observations,
not instructions.

A checkpoint carries Pi's own inherited Terminal id, the selected target, and
up to 64 sorted pane records: canonical Terminal/session/window identity,
agent label and kind, lifecycle state, attention, and cwd. When the CLI supplies
it, `agent_session` preserves the child log's resource selector, provider and
bounded native session id; absent and null remain distinct. This is an
AgentSession drill-in, not a durable coordinator Run. The complete message
is capped at 8 KiB and reports omitted panes. It never includes screen rows,
scrollback, titles, detector evidence, explanations, tool output, or
credentials. Use `phux_snapshot`, `phux_watch_events`, or `phux_panes` when
fresh explicit detail is required.

After eight deltas the next change becomes a full checkpoint. Branch movement
and compaction append a fresh checkpoint immediately; during an active
auto-compaction it is steered into the retried context, while an idle manual
compaction persists it without triggering a model turn. A missing or timed-out
server produces one bounded `unavailable` checkpoint; repeated identical failures emit nothing, and
recovery produces a new checkpoint. The refresh is best effort and locally
bounded to one second. It updates awareness at new user-turn boundaries, not
continuously during one uninterrupted model/tool loop. The cache and
compaction rationale is recorded in
[ADR-0067](../adr/0067-cache-preserving-agent-fleet-context.md).

## Selecting and preserving targets

`/phux` inventories the public agent projection, groups panes by session, and
stores the chosen canonical pane selector plus its owning session and window.
`phux_create` stores the same ownership fields for the newly created seed pane.
The selection is appended as the existing versioned `phux-target` custom entry
in Pi's session branch, preserving compatibility with earlier package sessions.

`phux_targets` adds named aliases and groups in a separate versioned branch
entry. Use `alias:build` anywhere a tool accepts one pane; `phux_kill` and
`phux_tag` also accept `group:workers` and expand it to at most 64 canonical
pane selectors. Definitions store pane ownership, not only `@id`. Immediately
before every named-target action, the extension refreshes inventory and rejects
missing or reused ids; inventory failure fails closed. Spatial operations also
require each role to resolve to exactly one distinct local pane and reject named
groups and satellite pane selectors. Read-only tools can pass explicit raw CLI
selectors to the CLI. Input, prompt, kill, signal, tag mutation, and ask tools
require canonical `@N` or `host/@N` targets after named-target resolution:
session, focus, wildcard, and other broad raw selectors are rejected because
they cannot safely exclude Pi's hosting pane. `PHUX_TERMINAL_ID` identifies
that pane; a matching resolved target is refused, including implicit targets,
aliases, and members of groups. An invalid inherited identity fails closed for
these writes. A group containing the parent is rejected before any member is
mutated. Without an inherited identity there is no known hosting pane to
exclude, but the canonical-target requirement remains.

Layout placement and insert/move/swap are topology operations, not terminal
input: placing a worker beside the hosting pane remains allowed. Branch
navigation reconstructs the latest selection and named-target document on that
branch.

Restoration never silently falls back to phux's focused pane. Before an
implicit target is made available to tools, the
extension confirms that the saved pane id still belongs to the saved session
and window. A missing pane or reused id is **stale**: the selection remains
visible for diagnosis, but an implicit targeted tool refuses it. An inventory
failure is **unavailable** and likewise preserves the saved selection. Pass an
explicit target to a tool only when intentionally overriding the selection.


## Lifecycle metadata

The extension resolves its inherited `PHUX_TERMINAL_ID` against the startup
inventory and reports identity only on that hosting pane, independently of
`/phux` control selection. No inherited host or no matching inventory pane means
no identity writes; it never labels a selected sibling as Pi. It reports a
`phux.agent/v1` record with `name=pi`, `kind=pi`, and a Pi-session owner in the
`session` field — **identity only, never a `state`**. A declared `state`
outranks the server's own derivation for the record's whole lifetime
([metadata protocol](../spec/L3.md) §3.7), so reporting one would stand the
`rules/pi.toml` detector down. Identity is written once per owner and target.

On a server that advertises `RESOURCE_KINDS`, the extension also opens one
AgentSession per pane and emits closed record types from Pi's lifecycle bus:
`session_start` at bind, `prompt` on `agent_start`, `tool_start` /
`tool_end` around tool execution, `ask` on a trust prompt or blocking UI
prompt, `stop` on `agent_settled`, and `session_end` then `session close` on
shutdown. The server derives working, blocked, and done from this stream.
If `phux agent session open` is missing or refused with `unsupported_server`,
emit fails closed; identity-only writes and the detector still run.

The same stream carries the conversation as `provider_raw` records in the
`phux.transcript/v1` convention
([ADR-0156](../adr/0156-agent-transcript-records.md)), so a phone can render
a native transcript while the pane's TUI stays the session: each user
message, the assistant reply (streamed as partials at most every 250 ms while
it grows, then final under the same id), visible thinking once a thinking
block ends, and each tool call as one entry keyed by its call id (running
with an argument summary, then ok or error). Text is cut
to keep each record under 16 KiB, and the text reaches `phux` on stdin, never
on a command line. Transcript records are on by default, because the pane
already shows the same text to the same clients; set
`PHUX_AGENT_TRANSCRIPT=0` before startup to turn them off. Tool output and
file contents are not on the screen, so tool entries carry an empty `output`
unless `PHUX_AGENT_TRANSCRIPT=full`, which adds the last 4 KiB.

Writes are serialized, debounced, and best-effort. Changing the selected control
target or navigating the session tree does not move the hosting declaration or
AgentSession. Shutdown clears the hosting declaration only after confirming Pi
still owns it. Reload adopts only a matching Pi-owned declaration and an
AgentSession whose provider and native session id match. It retains that exact
child resource for events and close: replacing the pane's live child cannot
redirect old events or cleanup to the replacement. Missing hosting state can be
established when the inventory explicitly reports no child; foreign or
unverifiable state is left untouched. Migration from an older selected-sibling
binding never cleans up that sibling implicitly. This is status metadata, not
an input lock.

## Current boundaries and security

- `phux_paste` uses the canonical paste CLI; it neither simulates keys nor
  appends Enter. `phux_send_keys` remains key input, not clipboard support.
- `phux_agent_prompt` requires the CLI's acknowledged-delivery capability.
  Unsupported servers and satellite prompt targets are refused by the CLI,
  never downgraded to fire-and-forget keys. Acknowledged admission is per
  pane: prompts to different panes run concurrently; serialize prompts to one pane.
- `phux_rendered_snapshot` follows the CLI's `snapshot --rendered` contract:
  unlike ordinary snapshot it attaches a headless client and establishes that
  client's bounded viewport. Use `phux_snapshot` for a side-effect-free pane
  read.
- `phux_launch` validates schema version 1, integration id, plugin id, terminal
  id, and resolved argv. It never returns the resolved argv to the model.
- Spawn/launch placement is local-only. `target`, `split`
  (`horizontal|vertical`), and `ratio` map directly to canonical CLI flags;
  satellite pane targets and `satellite` plus placement are rejected.
- Spatial tools parse the canonical schema-version-1 CLI JSON. Both role
  selectors are freshly ownership-validated when named aliases are used, and
  every subprocess preserves Pi cancellation, local timeouts, and output caps.
- The package is a Node/Pi integration around an external native process. It
  has no WASM build and does not render or nest a terminal inside Pi.
- Remote phux attach, pairing, and token transport are not supported. The
  adapter accepts a local Unix socket path, not `--quic`, `--ws`, bearer-token,
  or certificate arguments.
- Pairing tokens and certificate material are secrets: never place them in a
  prompt, tool argument, saved target, lifecycle record, or handoff.
  `/phux-attach` neither reads nor prints remote credentials.

A checked-in [live-fleet recording](../pi-live-fleet-proof.md) shows Pi using
this surface to place, drive, verify, and spatially rearrange real Claude Code
and OpenAI Codex panes. Package-local development and validation commands live
in the [Pi package development guide](../../integrations/pi/README.md).
