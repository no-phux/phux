---
audience: humans, consumers
stability: evolving
last-reviewed: 2026-09-21
---

# Cockpit

**TL;DR.** Cockpit is the native macOS attach client, independently versioned
from the CLI. It connects through `phux-client-ffi`; the server owns its
terminals and keeps them running when a window closes. This guide covers
installation, behavior, and client integration details.

---

## What it is

Cockpit is a native macOS application over the C ABI in `phux-client-ffi`,
not a wrapper around the TUI. It renders the server's Terminal-kind resources.
The app manages tabs, splits, focus, and input; the server runs the shells.

Closing a window detaches. Reopening reattaches to the session while its
server remains alive.

## Install

Apple silicon, macOS 11 or later. Intel Macs have no release artifact; the
curl installer and the cask both refuse there.

```sh
curl -fsSL https://phux.sh/install-cockpit | sh
phux cockpit
```

or the Homebrew cask:

```sh
brew trust --tap no-phux/tap
brew tap no-phux/tap
brew install --cask no-phux/tap/phux-cockpit
```

Full installer notes, including pinning a `cockpit-vX.Y.Z` tag and the
in-app Check for Updates path, live in [`../INSTALL.md`](../INSTALL.md).

## Authority

Cockpit has the same protocol standing as other clients; it does not own a
second multiplexer or process-running daemon. Releases use `cockpit-vX.Y.Z`
tags, independently of the `phux` binary.

AgentSession resources appear as rows under their parent terminal, not as
panes. See [Concepts](../CONCEPTS.md) for product maturity.

## Limits

- macOS Apple silicon only.
- Independently versioned from the CLI; install both when you want both.
- Direct local PTY sessions in the app are ephemeral and die with it.
  Durable work uses the phux server.

## Status effects

After each attach barrier releases, `phux-client-ffi` subscribes to the
connection-wide `AgentEvent` stream (`SUBSCRIBE_EVENTS`). Cwd changes,
command boundaries, process exits, and terminal closes become typed
`PhuxClientEffect` status kinds: `PHUX_CLIENT_STATUS_CWD`,
`_COMMAND_STARTED`, `_COMMAND_FINISHED`, and `_EXITED`
(`include/phux/client.h` in `phux-client-ffi`).

The subscription covers every Terminal local to the connected server.
Satellite panes behind a federation hub need explicit per-terminal
subscriptions. Missing status reads as unknown, not as an error.

An untitled tab uses the live working directory's basename (`/` for root)
before the attach catalog's name. `EXITED` with reason `Exited` or `Killed`
closes the pane; other reasons retain their existing handling. Natural
`exit` of a session's last shell does not publish `EXITED`: the server
replaces the child in place. Closing the last pane of a keep-empty session
shows Empty session (ADR-0114); otherwise the window closes. Closing the
last window quits. These outcomes do not depend on whether the close event
or workspace snapshot arrives first.

Command boundaries determine `atPrompt()`. A command that ran for at least
ten seconds posts a notification under the bell's gate and latch.

`PHUX_CLIENT_STATUS_INPUT_HOLDER` is the server's `terminal_control`
broadcast (ADR-0033): who holds a terminal's exclusive input lease, restated
on every lease or lifecycle change, with a flag for "this connection". Only
the holder's keys reach the PTY; a non-holder's are acked and dropped, so a
pane whose wheel another client holds must say so rather than look dead.
`phux_client_queue_acquire_input` (cooperative, or `seize` to preempt) and
`phux_client_queue_release_input` move the lease; both are correlated
operations. Taking over is a deliberate user act. Focus, a second attach, and
reconnect never acquire ([L1.md §8.1](../spec/L1.md)); the attach-time
alternative is `phux_client_attach_role` with `PRIMARY, DELIBERATE`.
Cockpit shows a wheel held elsewhere as the focused terminal's window
status, "Another client is driving", and Take Over Input (Shell menu and
the command palette) seizes it. Nothing is acquired on focus.

## Who owns the socket

`phux-client-runtime` owns the socket (ADR-0133). Cockpit names a destination;
the runtime resolves it through the CLI's `[[remote]]` registry and trust
rules, dials, reconnects, and handles socket I/O on its own thread. It wakes
Cockpit to call `phux_client_poll`.

Cockpit's owning thread decodes frames because the ABI's per-frame behavior
uses state confined to that thread. Each `poll` drains one snapshot of the
bounded runtime queue. Contiguous terminal output reaches the engine in
batches of at most 256 frames or 1 MiB, publishing each grid once per batch.

- The runtime queues `HELLO` on every connection it opens. `ATTACH` stays
  explicit. Both a changed `phux_client_connection_epoch` and replacement
  of the ABI handle retire Cockpit's connection generation and clear its
  attach/catalog-request latches. Header Reconnect may replace the handle;
  automatic reconnect normally retains it.
- Cockpit retains the selected session's confirmed name and server identity.
  It reuses a numeric ID only on the same server incarnation; after server
  replacement it attaches that exact name, never a recycled ID or an implicit
  new session. Frozen pixels do not grant input authority, and held input is
  not replayed into the replacement coordinator.
- A bare `host:port` endpoint has no lane and is refused: the registry is
  what carries the certificate pin and token a routable dial needs.

`phux_client_feed_frame` and `phux_client_outgoing_*` remain as the
embedded lane that Cockpit's tests stage exact frames through; nothing in
production drives it.

## Terminal theme

`phux_client_set_terminal_theme` installs a theme's palette 0..15 and its
default foreground, background, and cursor (ADR-0157) as the defaults of
every remote replica, now and after every reconnect; NULL clears them.
They are defaults, not overrides: a program's OSC 4/10/11/12 still wins,
and OSC 104/110/111/112 revert to the theme. Visible terminals republish,
and `PhuxTerminalGridMetadata.palette` carries the result. Fill the struct
from a `colors.toml` with the fixed mapping: 0 `background`, 1..6 `red`
`green` `yellow` `blue` `magenta` `cyan`, 7 `foreground`, 8 `muted`,
9..14 the `bright_*` variants, 15 `bright_foreground`, cursor
`bright_foreground` (`phux theme show NAME --json` prints exactly this as
`ansi16`). Cleared, `has_foreground` / `has_background` report false
again and Cockpit's own fallback applies.

## Clipboard (OSC 52)

OSC 52 (the terminal-to-host clipboard-write escape) is governed by
[ADR-0158](../adr/0158-pane-clipboard-writes-are-client-policy.md): the client
focused on the pane honours it under `defaults.clipboard-write`. Cockpit does
not deliver it yet and drops it; the TUI does.

## Build

Build instructions live in the
[in-tree app README](../../clients/cockpit/README.md).
