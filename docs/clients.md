---
group: connect
order: 2
---

# Clients

The programs that show sessions from a phux server. All speak the same
[protocol](./protocol.md), so several can show one session at once.

## Terminal UI

`phux` and `phux attach` run the terminal UI in your current terminal. It
ships in every release and is the reference client; it draws panes with
[libghostty](./libghostty.md), the emulator the server uses.
[Getting started](./getting-started.md) covers it.

A document, window mirror, accessibility tree, or agent pane another client
placed in the arrangement ([resource kinds](./resource-kinds.md)) shows as a
tile naming its kind and title. Typing there does nothing. Killing such a
pane only closes the view; `reopen-views` shows a hidden document again.

`phux rec` records a pane to a cast, GIF, or APNG; `phux play` plays one in a
new pane.

## Native terminal windows

`phux native create` shows one server-owned terminal in your emulator's own
window, with no sidebar, tabs, status bar, or layout; your window manager
tiles the windows.

```sh
phux native create                      # a new terminal in session omarchy
phux native create -s work -c ~/src -- bash
phux ls omarchy
phux native attach @3                   # reattach by exact address
```

Each `create` makes a distinct terminal. Ctrl-] detaches and the process
keeps running; Ctrl-C and Ctrl-Z go to the program; closing the window
leaves the shell running. Rune and the CLI see and manage the same
terminals when on the same server. Native clients never change layout or
focus. One PTY has one size, so the last resize wins.

Limits: local sockets only, VT output rendered by the emulator (image and
scrollback fidelity follow the compatibility emitter), and no automatic
reconnect: reattach after a connection failure.

## Desktop

[Phux Rune](../clients/rune/README.md) is the desktop app, an alpha client
for macOS 14+ (Apple silicon) and x86_64 or arm64 Linux with Vulkan and
glibc 2.39:

```sh
curl -fsSL https://phux.sh/install | sh -s -- --rune
```

It installs the CLI if missing, starts or reuses the server, and updates
itself. It never creates sessions or stops the server on its own; quitting
detaches. It shows every session and agent state on this machine and every
added machine, draws documents, trees, and pixel surfaces, sends input to
mirrors, and has a command palette, the server-stored splits, find, Git
changes, and popups. It ships as signed `rune-v*` releases. The same app has
a [phone form](../clients/rune/MOBILE.md). On Windows, run the CLI and
server in WSL 2.

## Browser

An experimental browser client in the repository draws a session with
libghostty compiled to WebAssembly over WebTransport or WebSocket. It is not
shipped and the server's browser listeners are off by default. **A browser
needs an enrolled client certificate to reach a paired server**, which
WebTransport can't present, so use WebSocket.

## Phones

The Android app is in open testing on
[Google Play](https://play.google.com/store/apps/details?id=dev.phux.mobile);
iOS is in testing. Scan `phux pair --qr`
([pairing with a link](./remote.md#pairing-with-a-link)). Off your network,
pair through a [relay](./remote.md#relays) with
`phux pair --relay-route home`.

## Host colours

Each machine gets one of eight palette colours hashed from its name, the
same on every client. Amber, mint, and red stay reserved for agent state.
Override one:

```sh
phux host color mini violet
phux host color --local sky
phux host color mini auto        # back to the hash
```

## Host themes

A whole theme says which machine you are on louder than a colour. Name one
on a host's entry and Rune and the terminal UI wear it while you view that
machine (attached to it, or a hub's satellite pane of that name), and go
back to your own theme when you leave:

```toml
[[remote]]
name = "prod"
theme = "red-alert"   # as `phux theme list` (terminal UI) or Rune's picker names it

[host]
theme = "nord"        # this machine's own panes
```

Each client looks the name up in its own themes; one it does not have
keeps your theme. Unset, nothing switches.

## Pane icons

`[[icons]]` puts an icon before a pane's name by foreground `process` or
`cwd` glob, and `[[sessions]]` gives a session a palette colour
([config reference](./reference/config.md)). An icon a program sets with
[`OSC 1337 SetUserVar=phux_icon`](./program-sequences.md#pane-icon-osc-1337-setuservar)
wins. Both live in your config, not on the server: `phux config reload`
applies an edit to the terminal UI; Rune reads them at start.

## Your own

[Embed a terminal](./embedding.md) puts a phux terminal in your app.
