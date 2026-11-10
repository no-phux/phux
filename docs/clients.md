# Clients

A client is a program that shows sessions from a phux server: the terminal
UI, the desktop app, or something you build. Every client talks to the same
server protocol, so several clients can show one session at once. Individual
clients differ in supported features and maturity.

The terminal UI is the core client. The separately released desktop is
maintained but newer; the browser client is experimental and unshipped.
Phone apps are not available yet. Remote hosts and agent integrations are
optional; no extra client is required to use the server from the CLI.

```sh
phux attach work        # the terminal UI, in this terminal
```

## Terminal UI

`phux` and `phux attach` run the terminal UI in whatever terminal you are
in. It ships in every release, and every other client is measured against
it. It draws panes with libghostty, the same terminal emulator the server
uses, so what you see is what the server has. See
[libghostty in phux](./libghostty.md).

A session's arrangement can also hold a document, window mirror,
accessibility tree, or agent pane that another client placed there
([resource kinds](./resource-kinds.md)). The terminal UI shows each as a
tile naming its kind and title; it can take focus, but typing there does
nothing and the tile is never resized. Killing such a pane, or a tab
holding one, only closes its view: the resource and its producer keep
running, and `reopen-views` shows a hidden document again.

[Getting started](./getting-started.md) covers this client.
<kbd>C-a</kbd> <kbd>S</kbd> opens settings, which edit your config file;
[when a change takes effect](./config.md#when-a-change-takes-effect)
explains when you see it.

`phux rec` records a pane to a cast, GIF, or APNG. A recording only watches
output; it is not a backup of the process. `phux play` plays one back in a
new pane and never overwrites an existing one.

## Native terminal windows

`phux native create` displays a distinct terminal in a native emulator window,
without the TUI sidebar, tabs, status bar, or pane layout. The default shared
session is `omarchy`; the window manager tiles the windows.
[Native terminal windows](./native-terminal.md) covers creation, reattachment,
detach, and the client's current limits.

## Desktop

The desktop app is [Phux Rune](../clients/rune/README.md), an alpha client
built on the [Rune](https://github.com/anomalyco/rune) UI framework, for macOS
14 or later on Apple silicon and x86_64 or arm64 Linux with Vulkan and glibc
2.39:

```sh
curl -fsSL https://phux.sh/install | sh -s -- --rune
```

`--desktop` and `https://phux.sh/install-desktop` install the same app. It
installs the CLI when none is found, starts or reuses the server, and updates
itself. It never creates sessions on its own or stops the server; quitting
detaches. Windows has no native desktop app: the CLI and server run in WSL 2,
with PowerShell launchers.

It shows every session and terminal with each agent's state, on this machine
and on each machine you added with `phux host add` or **Add a machine**. It
also draws documents, accessibility trees, and pixel surfaces as panes,
sends input to a mirrored page, window, or display, and starts mirrors from
its palette ([resource kinds](./resource-kinds.md#available-now)). It has
a command palette, splits from the session's server-stored arrangement (the
same layout the terminal UI shows), find in history, ⌘-clickable links, an
agent inbox with notifications, tabs, and themes. It ships as `rune-v*`
releases, Developer ID signed and notarized on macOS, and does not draw Kitty
images yet. The same app has a phone form ([MOBILE.md](../clients/rune/MOBILE.md)).
Expect rough edges: the terminal UI is ahead of the desktop.

The previous GPUI desktop was retired after Desktop 0.62.0; its source is the
`desktop-v0.62.0` tag (`git checkout desktop-v0.62.0 -- clients/desktop`).

## Browser

A browser client exists in the phux repository. It draws a session on a
canvas with libghostty compiled to WebAssembly, over WebTransport or
WebSocket. It works, but it isn't shipped: nothing serves it, and the
server's browser listeners are off by default. It draws terminals only;
documents, trees, and surfaces in a session are not shown. Use it as a
starting point for putting a terminal in a web page.

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
