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
uses, so what you see is what the server has.

[Getting started](./getting-started.md) covers this client.
<kbd>C-a</kbd> <kbd>S</kbd> opens settings, which edit your config file;
[when a change takes effect](./config.md#when-a-change-takes-effect)
explains when you see it.

`phux rec` records a pane to a cast, GIF, or APNG. A recording only watches
output; it is not a backup of the process. `phux play` plays one back in a
new pane and never overwrites an existing one.

## Desktop

The phux desktop is a native app for macOS 26.0 or later on Apple silicon,
Linux on x86_64 or arm64 with Vulkan, and Windows x64 with a WSL 2 server.
On macOS and Linux, install it with:

```sh
curl -fsSL https://phux.sh/install-desktop | sh
```

Windows: the CLI and server run in WSL 2, with PowerShell launchers. The
[native x64 desktop](../clients/desktop/README.md#windows) uses Direct3D 11,
Windows input/clipboard, and the same WSL coordinator. Windows x64 ZIPs ship
from [Desktop 0.50.0](https://github.com/no-phux/phux/releases/tag/desktop-v0.50.0)
onward; older desktop tags have no Windows download. WSLg is not needed.
Updates are manual on Windows, and the local Changes panel and external
Unix developer socket are unavailable there.

It shows every session on this machine and on each machine you added with
`phux host add` or the app's **Add Machine…**, grouped by machine or by
repository. Add Machine lists the machines on your Tailscale network and
installs phux on the one you pick ([Remote machines](./remote.md#machines-on-your-tailnet)). One window shows one
session, and the sidebar switches between them. Agents that need you are
marked where they are. <kbd>⌘P</kbd> opens a palette of sessions, actions,
and settings. On macOS and Linux the app checks for updates and offers to
install them on restart; Windows updates are manual.
An update reopens the same windows, hosts, sessions, tabs and focused panes,
bringing the foreground window back to the front.

Each window keeps one live connection per machine, so switching back to a
session you just left is instant. Machines you aren't looking at keep their
data current without drawing anything. Disconnecting, forgetting a machine,
or closing the window closes its connections.

On macOS the app bundles the CLI/server. On Linux, install the CLI first
or use the desktop installer, which supplies it when none is found.
The app never creates sessions on its own or stops the server. Quitting detaches.

Terminal panes and previews render inline Kitty graphics: PNG and raw RGB/RGBA,
chunked transfers, classic placements and Unicode placeholders, cropping,
transparency and z-order. Visible images survive attach and reconnect through
synthesized terminal replay. File, temporary-file and shared-memory transfers are
disabled; applications must send image bytes inline. Sixel, Kitty animations and
relative placements are not supported. Image-number (`I=`) references are not
preserved across a synthesized reattach; use explicit image IDs (`i=`).
Classic placements outside the captured bootstrap viewport are not reconstructed;
resident virtual prototypes remain available to Unicode placeholders in history.

For `icat`, use `kitten icat --transfer-mode=stream image.png`
([transfer modes](https://sw.kovidgoyal.net/kitty/kittens/icat/#cmdoption-kitty-kitten-icat-transfer-mode)).

- [Quick Capture](./desktop-capture.md) turns a thought into a new agent
  terminal.
- The [Changes panel](./desktop-changes.md) reviews and stages Git changes
  beside your panes.
- [Reviews](./desktop-reviews.md) lists your open GitHub pull requests and
  reviews, merges or checks out each one.
- [Agent panes](./desktop-panes.md) explains how panes an agent drives look.

The desktop app has its own version and release notes. The macOS app and
DMG are Developer ID signed, Apple-notarized, and stapled. The desktop app
requires macOS 26.0; the standalone CLI requires macOS 11.0. Expect rough
edges: the terminal UI is ahead of the desktop.

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
