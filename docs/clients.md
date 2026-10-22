# Clients

A client is a program that shows sessions from a phux server: the terminal
UI, the desktop app, or something you build. Every client talks to the same
server the same way, so any client can show any session, and several can
show one session at once.

```sh
phux attach work        # the terminal UI, in this terminal
```

## Terminal UI

`phux` and `phux attach` run the terminal UI in whatever terminal you are
in. It ships in every release, and every other client is measured against
it. It draws panes with libghostty, the same terminal emulator the server
uses, so what you see is what the server has.

[Getting started](./getting-started.md) covers this client.
<kbd>C-a</kbd> <kbd>S</kbd> opens settings, which edit your config file;
[when a change takes effect](./config.md#when-a-change-takes-effect)
explains when you see it.

`phux rec` records a pane to a cast, GIF, or APNG. A recording only watches
output; it is not a backup of the process. `phux play` plays one back in a
new pane and never overwrites an existing one.

## Desktop

Phux Desktop is a native app for macOS 27 or later on Apple silicon, and for
Linux on x86_64 or arm64 with Vulkan. Install it with:

```sh
curl -fsSL https://phux.sh/install-desktop | sh
```

It shows every session on this machine and on each machine you added with
`phux host add`, grouped by machine or by repository. One window shows one
session, and the sidebar switches between them. Agents that need you are
marked where they are. <kbd>⌘P</kbd> opens a palette of sessions, actions,
and settings. The app checks for updates and installs them on restart.

Each window keeps one live connection per machine, so switching back to a
session you just left is instant. Machines you aren't looking at keep their
data current without drawing anything. Disconnecting, forgetting a machine,
or closing the window closes its connections.

The app starts or reaches the server through the installed `phux` command,
so install the CLI first; the desktop installer does it if you haven't. The
app never creates sessions on its own and never stops the server. Quitting
it detaches.

- [Quick Capture](./desktop-capture.md) turns a thought into a new agent
  terminal.
- The [Changes panel](./desktop-changes.md) reviews and stages Git changes
  beside your panes.
- [Agent panes](./desktop-panes.md) explains how panes an agent drives look.

The desktop app has its own version and release notes. The macOS build is
signed ad hoc, not notarized. Expect rough edges: the terminal UI is ahead
of it.

## Browser

A browser client exists in the phux repository. It draws a session on a
canvas with libghostty compiled to WebAssembly, over WebTransport or
WebSocket. It works, but it isn't shipped: nothing serves it, and the
server's browser listeners are off by default. Use it as a starting point
for putting a terminal in a web page.

**A browser needs a client certificate to connect to a paired server.**
Over WebSocket, install an enrolled client certificate in the browser.
WebTransport from a browser can't present one, so a paired server refuses
it. A token alone never gets a browser in.

## Phones

iOS and Android apps are in progress and not available yet. `phux pair --qr`
already prints the pairing code they will scan; see
[pairing with a link](./remote.md#pairing-with-a-link).

## Your own

To put a phux terminal in your own app, see
[Embed a terminal](./embedding.md). [Protocol](./protocol.md) describes what
clients send and receive.
