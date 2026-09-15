---
audience: humans, consumers
stability: evolving
last-reviewed: 2026-09-14
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
which need their own explicit per-terminal subscription. Cockpit does not
consume these yet — it still reads a pane's working directory from the
attach catalog (`workspace_bridge.zig`) — wiring them into the app is
PHA-284, a separate change.

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
