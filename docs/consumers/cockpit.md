---
audience: humans, consumers
stability: evolving
last-reviewed: 2026-09-21
---

# Cockpit

**TL;DR.** Cockpit is the native macOS attach client. It talks to the same
phux server the TUI does, through `phux-client-ffi`, so the terminals you
split in one are the terminals you open in the other. It is independently
versioned. This page is the human router; the in-tree app owns its build
notes.

---

## What it is

Cockpit is a native macOS application over the C ABI in `phux-client-ffi`.
It does not run the TUI. It attaches to a running phux server and paints
the same Terminal-kind resources the TUI shows: tabs, splits, focus, and
input stay in the app; the shells stay in the server.

Closing a Cockpit window detaches. The session keeps running. Reopening
reattaches to the same durable work.

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

Same server, same terminals as the TUI. Cockpit is a peer consumer, not a
second multiplexer and not a second process-owning daemon. Its release
cadence is independent of the CLI: `cockpit-vX.Y.Z` tags, not the `phux`
binary's.

This tree projects AgentSession resources as rows under their parent
terminal, not as panes. Maturity of the rest of the product is in
[`../CONCEPTS.md`](../CONCEPTS.md).

## Limits

- macOS Apple silicon only.
- Independently versioned from the CLI; install both when you want both.
- Direct local PTY sessions in the app are ephemeral and die with it.
  Durable work uses the phux server.

## Status effects

`phux-client-ffi` subscribes to the connection-wide `AgentEvent` stream
(`SUBSCRIBE_EVENTS`) once every attach barrier releases, and folds the
subscribed cwd change, command-boundary, and process-exit events — plus a
plain terminal close — into typed `PhuxClientEffect` status kinds:
`PHUX_CLIENT_STATUS_CWD`, `_COMMAND_STARTED`, `_COMMAND_FINISHED`, and
`_EXITED` (`include/phux/client.h` in `phux-client-ffi`). The subscription
is scoped to every Terminal local to the server phux-client-ffi is
connected to; on a federation hub it does not reach a satellite's panes,
which need their own explicit per-terminal subscription. Cockpit consumes
them on the remote provider (PHA-284). The live working directory's basename
(`/` for the root) names an untitled tab ahead of the attach catalog.
`EXITED` with reason `Exited` or `Killed` closes the pane; any other reason
keeps today's handling. Natural `exit` of a session's last shell does not
publish `EXITED`: the server replaces the child in place, so the pane stays.
A window that loses its last pane to Close Pane/Tab shows Empty session when
its session is keep-empty (ADR-0114). Otherwise the window closes, and closing
the last window quits. The result is the same whichever
of the close and the workspace snapshot arrives first. The command
boundaries answer `atPrompt()`. A command that ran at least ten seconds posts
a notification under the bell's gate and latch. A missing status, such as a
satellite pane behind a hub, reads as unknown, never as an error.

## Who owns the socket

Two lanes, one C ABI (ADR-0133).

The **embedded lane** is the shipping default and what every Zig test
drives: Cockpit's own worker dials, reconnects, and pumps frames through
`phux_client_feed_frame` and `phux_client_outgoing_*`. A remote host is
reached by handing one end of a Unix-domain socket pair to a tunnel.

The **connected lane** hands all of that to `phux-client-runtime`. It
resolves the target through the CLI's `[[remote]]` registry under the CLI's
trust rules, dials, walks the reconnect ladder, and reads and writes the
socket on its own thread; Cockpit hands it a target, gets woken, and calls
`phux_client_poll`. Frames are still decoded on Cockpit's owning thread, so
the ABI's per-frame behavior is unchanged.

The connected lane is opt-in while it waits for live acceptance:

```sh
PHUX_COCKPIT_CONNECTED=1 phux cockpit
```

On that lane the runtime queues `HELLO` itself, `phux_client_connection_epoch`
replaces "a new client per connection" as the reconnect fence, and a bare
`host:port` endpoint stays on the worker, because the registry is what
carries the pin and token a routable dial needs.

## Clipboard (OSC 52)

OSC 52 (the in-band terminal-to-host clipboard-write escape) is
deliberately unsupported on the remote attach path: a pane running on a
remote phux server writing the local clipboard of whatever machine Cockpit
happens to be running on crosses the same trust boundary a remote
filesystem write would. No status effect carries it, and none is planned;
lifting this would need its own ADR, not a new effect kind.

## Build

The in-tree app, including how to build it, is
[`../../clients/cockpit/README.md`](../../clients/cockpit/README.md).
