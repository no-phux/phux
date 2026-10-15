---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-09-12
---

# The internal Rust client library

**TL;DR.** `phux-client` is the unpublished, in-tree Rust library behind
the CLI and MCP adapter. Its programmatic control surface is a set of
async free functions for selection, snapshots, input, commands, waits,
events, and asks; it has no typed client facade or stable public SDK
contract. External callers should use the CLI or MCP adapter.

---

## What it is

`phux-client` is workspace-internal (`publish = false`). It wraps the
`phux-protocol` codec and the selection, snapshot, run, and wait functions
behind the CLI agent verbs and [MCP adapter](./mcp.md).

Native embedders use `phux-client-ffi`: the C ABI or UniFFI for
phux-mobile. Both project `phux-client-runtime`, which owns dialing,
reconnect, the session kernel, acknowledged-input replay, and grid publication.
See the [client runtime](../architecture/client-runtime.md) (ADR-0133,
ADR-0135).

`phux-client` speaks L1 ([`../spec/L1.md`](../spec/L1.md)); its session
and arrangement helpers use the organization commands
([`../spec/L2.md`](../spec/L2.md)) and fall back to the L3 conventions
([`../spec/L3.md`](../spec/L3.md)) on an older server, not a privileged tier
([ADR-0017](../adr/0017-tui-not-protocol-privileged.md)).

## How it fits the projection thesis

`phux-client` reads structured screen state through the server's
engine-convenience snapshots (`GET_SCREEN` / `GET_TERMINAL_STATE`) rather
than running a local engine. These snapshots are not a normative structured
wire tier ([ADR-0030](../adr/0030-engine-delegated-wire-and-projection-consumers.md)).

For a client that renders its own grid, follow [phux-web](./web.md): Rust
compiled to WASM loads `ghostty-vt.wasm` and projects terminal bytes locally.

## Free-function surface

There is no `Agent` handle. The CLI and MCP adapter compose the crate's
module functions directly:

- `selector::{parse, resolve, resolve_with_tags, pick_target_pane}`
  parses the CLI target grammar and resolves it against a
  `SessionSnapshot`.
- `snapshot::{get_screen, get_screen_scrollback}` reads structured
  screen state without attaching or resizing the terminal.
- `send_keys::{send, send_to}` routes input to a focused or
  already-resolved terminal.
- `run::{run, run_in}` submits a command and returns a `RunOutcome`
  with its captured `RunResult` or timeout state.
- `wait::poll_until` polls screen state for a `Condition` and returns a
  `WaitResult`.
- `watch::watch_events` consumes the pushed `AgentEvent` stream.
- `ask::report` reports an `AskedPayload` to the existing event stream.

The async operation functions open the connections they need and return
`attach::AttachError` for transport, protocol, or server refusal
failures. The selector helpers are synchronous and operate on
caller-provided snapshots. Outside the workspace, use the versioned JSON
surface described in the [agent CLI guide](./agents.md) and
[MCP adapter](./mcp.md).

The JSON shapes and exit codes are [`agents.md`](./agents.md); the codec is
[`../spec/appendix-encoding.md`](../spec/appendix-encoding.md).

## Native status effects

After each attach barrier releases, `phux-client-ffi` subscribes to the
connection-wide `AgentEvent` stream (`SUBSCRIBE_EVENTS`). Cwd changes,
command boundaries, process exits, and terminal closes become typed
`PhuxClientEffect` status kinds: `PHUX_CLIENT_STATUS_CWD`,
`_COMMAND_STARTED`, `_COMMAND_FINISHED`, and `_EXITED` in
`include/phux/client.h`. The subscription covers Terminals local to the
connected server; satellite panes need explicit per-terminal subscriptions.
Missing status is unknown, not an error.

`PHUX_CLIENT_STATUS_INPUT_HOLDER` reports the server's input-lease holder.
Use correlated `phux_client_queue_acquire_input` and
`phux_client_queue_release_input` operations to move the lease. Focus and
reconnect never acquire it implicitly (L1 §8.1).

## Native terminal theme

`phux_client_set_terminal_theme` installs the default ANSI-16 palette,
foreground, background, and cursor for every remote replica, including
after reconnect; NULL clears them. Program OSC color changes still win.
`PhuxTerminalGridMetadata.palette` carries the resolved palette.

The `colors.toml` mapping is: 0 `background`, 1..6 `red`, `green`, `yellow`,
`blue`, `magenta`, `cyan`, 7 `foreground`, 8 `muted`, 9..14 the `bright_*`
variants, and 15 `bright_foreground`. The cursor is `bright_foreground`.
`phux theme show NAME --json` prints this as `ansi16`.
