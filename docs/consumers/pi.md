---
audience: humans, agents, consumers, contributors
stability: evolving
last-reviewed: 2026-08-02
---

# Pi integration

**TL;DR.** `@phux/pi` lets Pi select and operate panes in an external local
phux server, preserves branch-local targets, and appends bounded fleet-state
checkpoints and deltas without rewriting Pi's stable prompt prefix. It provides
nineteen terminal tools, three human commands, and best-effort lifecycle
metadata. It does not embed a terminal or own the server.

---

## Requirements and installation

The package requires Node.js 20 or newer, Pi, and an external `phux` executable
on `PATH`. It does not bundle phux or start a separate implementation. The
minimum compatible binary is `phux 0.1.0`; check it before loading the package:

```sh
phux --version
```

A trusted checkout of this repository loads its local package automatically
through [`.pi/settings.json`](../../.pi/settings.json), so a fresh clone has the
integration without a separate install step. If `@phux/pi` is already configured
in the user's global Pi settings, the checkout yields to the first global copy
instead of registering duplicate tools. This lets contributors keep a preferred
or in-development global checkout while preserving the out-of-box project
integration for everyone else.

To use this checkout's package in other repositories, install the package
directory, not the git root:

```sh
pi install ./integrations/pi
```

The repository root is not a Pi package, so a git source that resolves to that
root is not an installation method for this integration. `@phux/pi` is also not
published to npm today. Do not use `npm:@phux/pi` unless a future release is
actually present in the registry.

A packed artifact can be tested or moved to another machine without implying
registry publication:

```sh
cd integrations/pi
npm ci
npm pack
mkdir -p phux-pi-packed
tar -xzf phux-pi-0.1.0.tgz -C phux-pi-packed
pi install ./phux-pi-packed/package
```

`npm pack` runs the package build. Pi installs the extracted package directory;
it does not load the `.tgz` file as an extension. The artifact remains dependent
on a compatible external `phux` binary on the destination machine.

The extension inherits `PHUX_SOCKET`; set it before starting Pi when the server
uses a non-default local Unix socket. It also reads `PHUX_TERMINAL_ID`, which
phux sets for a hosted agent, to identify Pi's own pane in automatic fleet
context. Set `PHUX_CONTEXT_AWARENESS=0` before startup to disable that context.
There is no package command for choosing an alternate executable. Library
consumers can construct `PhuxCli` with an absolute `executable`, but the
installed Pi extension expects `phux` on `PATH`.

## Surface

The extension registers exactly these nineteen model tools:

| Tool | Operation |
|---|---|
| `phux_list` | List phux sessions. |
| `phux_create` | Create a named session without attaching and select its seed pane. |
| `phux_snapshot` | Read a pane's bounded, side-effect-free screen projection. |
| `phux_send_keys` | Send named keys or literal key text to one pane. |
| `phux_run` | Run one shell command line and return its exit result. |
| `phux_wait` | Wait for visible text or idleness and return the bounded final screen. |
| `phux_panes` | Inventory pane ownership, agent state, attention, title, cwd, and evidence. |
| `phux_spawn` | Spawn a pane without attaching, optionally place it beside one exact local pane, and optionally save an alias. |
| `phux_launch` | Launch a configured integration from the CLI's versioned machine result, with optional local placement. |
| `phux_insert_pane` | Insert one already-created exact local pane beside another. |
| `phux_move_pane` | Move one exact local pane beside another, including across sessions. |
| `phux_swap_pane` | Swap two exact local pane leaves without changing geometry. |
| `phux_kill` | With an explicit target and `confirm:true`, destroy a selector, alias, or the validated members of a named group. Selectors and groups may destroy multiple panes. |
| `phux_signal` | Interrupt, freeze, or resume a pane's process group; terminate and kill require an explicit target and `confirm:true` because selectors may affect multiple processes or panes. |
| `phux_tag` | List, add, or remove terminal tags. |
| `phux_ask` | Report a human-attention ask event. |
| `phux_watch_events` | Collect typed events for 50 ms–30 s, then stop the streaming CLI subprocess. |
| `phux_rendered_snapshot` | Capture the attached client's composited frame at bounded dimensions. |
| `phux_targets` | List or mutate branch-local named aliases and groups. |

It registers exactly these three human commands:

| Command | Operation |
|---|---|
| `/phux` | Inventory panes and choose the default target. |
| `/phux-status` | Refresh and report the saved target and its availability. |
| `/phux-attach` | Print a human attach argv; it never executes the attach. |

The headless phux CLI owns argument syntax, selector rules, JSON, and exit
codes. Use the [agent CLI guide](./agents.md) for that canonical contract rather
than treating this adapter as a second CLI definition.

Input-authority control is an upstream blocker rather than a Pi tool. The
`take` lease is scoped to a live CLI connection, and a one-shot CLI invocation
disconnects immediately and releases it; `give` consequently cannot represent
a durable paired action either. These tools must remain absent until phux
provides a persistent transport/lifetime that Pi can safely own.

Tool output sent to the model is bounded to 200 lines and 12 KiB. CLI stdout
and stderr capture are independently bounded, every subprocess accepts Pi
cancellation, and targeted CLI tools expose finite local timeouts. The watch
adapter requires a finite collection window and returns at most 100 parsed events rather than
leaving an indefinitely streaming subprocess. Results state when the adapter
truncated output and preserve a separate truncation flag reported by phux.

## Automatic fleet context

Before each new Pi agent run, the extension reads the public agent inventory.
The first observation is a hidden `phux-context` checkpoint appended after the
new user message. Later changes append sequenced deltas; an unchanged inventory
adds no message. The base system prompt, context files, and tool definitions
therefore remain an exact stable prefix. The model is told that the latest
sequence supersedes older phux context and that every value is untrusted
observational data rather than an instruction.

A checkpoint carries Pi's own inherited Terminal id, the selected target, and
up to 64 sorted pane records: canonical Terminal/session/window identity,
agent label and kind, lifecycle state, attention, and cwd. The complete message
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
[ADR-0067](../../ADR/0067-cache-preserving-agent-fleet-context.md).

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
groups and satellite pane selectors. Explicit raw CLI selectors are caller-owned
for ownership and are still subject to the canonical CLI's exact-one validation. Branch
navigation reconstructs the latest selection and named-target document on that
branch.

Restoration never silently falls back to phux's focused pane. Before an
implicit target is made available to tools or lifecycle reporting, the
extension confirms that the saved pane id still belongs to the saved session
and window. A missing pane or reused id is **stale**: the selection remains
visible for diagnosis, but an implicit targeted tool refuses it. An inventory
failure is **unavailable** and likewise preserves the saved selection. Pass an
explicit target to a tool only when intentionally overriding the selection.

## First shared-terminal walkthrough

1. Start or locate a local phux server outside Pi. For a first local session,
   `phux new work` creates and attaches interactively.
2. Start Pi with the package installed and run `/phux`.
3. Choose the pane under `work`. The Pi status line shows the saved target.
4. Ask Pi to create or launch a worker beside that target. For example,
   `phux_launch({ integration: "codex", target: "@3", split: "vertical",
   ratio: 0.4, alias: "worker" })` creates a side-by-side pane. `split` or
   `ratio` requires `target`; ratios must be finite and strictly between 0 and
   1. `vertical` means side-by-side and `horizontal` means stacked.
5. Shape already-created panes with `phux_insert_pane`, `phux_move_pane`, or
   `phux_swap_pane`. These mutate persisted topology only: they do not spawn,
   focus, take, give, or paste. Insert and move accept optional `direction` and
   `ratio`; horizontal is the CLI default. Insert never spawns its `new_pane`.
6. Ask Pi to inspect the pane or run a discrete command. Pi can use
   `phux_snapshot`, `phux_run`, and `phux_wait` without attaching or resizing
   the human view.
7. Run `/phux-status` before a handoff if the pane may have exited or moved.
8. Run `/phux-attach`. Pi prints an argv such as
   `["phux","attach","work"]` and identifies the pane to navigate to after
   attach. Copy the argv into a separate real terminal. The extension does not
   execute it and does not open a nested terminal inside Pi.

A human attach joins the same terminal state. It is not a read-only monitor:
agent input and human input can interleave at the PTY. The package does not
serialize independent writers, reserve a prompt, or provide a transaction
around `snapshot` followed by `send_keys`. Coordinate before typing into a pane
that the other participant is actively driving, and use explicit targets when
several panes are live.

## Lifecycle metadata

When a target is ownership-validated as available, the extension reports a
`phux.agent/v1` record with `name=pi`, `kind=pi`, and a Pi-session owner in the
`session` field — **identity only, never a `state`**. A declared `state`
outranks the server's own derivation for the record's whole lifetime
([`../spec/L3.md`](../spec/L3.md) §3.7,
[ADR-0046](../../ADR/0046-server-side-agent-state-detection.md) point 8), so
reporting one would stand the shipped `rules/pi.toml` detector down on every
pane running this extension. Pi's per-turn `agent_start` / `agent_settled`
events are therefore not subscribed at all: the server derives `working` and
`blocked` from the manifest, and a per-turn identity rewrite would itself
clobber that derivation, since a whole-record write carries `state: "unknown"`.

Only a change of owner or target writes. Writes are serialized, debounced,
locally bounded, and best-effort, so a missing server does not break Pi startup
or shutdown.

A target switch clears the old declaration only after reading it back and
confirming that Pi still owns it. Normal shutdown applies the same ownership
check. Extension reload preserves the declaration for the replacement
instance, avoiding a clear/set flicker. This is status metadata, not an input
lock; it does not prevent the interleaved-input case above.

## Current boundaries and security

- There is no paste tool. The current canonical CLI ships no headless paste
  verb, so the adapter does not synthesize paste from `send-keys` or bypass the
  CLI. `phux_send_keys` remains key input only; dedicated paste is tracked by
  bead `phux-foir`.
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
- Pairing tokens and certificate material are secrets. Never place them in a
  Pi prompt, tool argument, saved target, lifecycle record, attach handoff, or
  smoke-test output. Configure remote access outside this package and expose a
  suitable local phux endpoint only if its trust boundary is understood.
- `/phux-attach` deliberately emits only local attach argv plus session/pane
  navigation. It neither reads nor prints remote credentials.

A checked-in [live-fleet recording](../pi-live-fleet-proof.md) shows Pi using
this surface to place, drive, verify, and spatially rearrange real Claude Code
and OpenAI Codex panes. Package-local development and validation commands live
in [`../../integrations/pi/README.md`](../../integrations/pi/README.md).
