# Clients

Everything talks to the same server over the same protocol, so any client
can show any session, and several can show one at once.

## Terminal UI

`phux` and `phux attach` run the terminal UI in whatever terminal you are
in. It is the reference client and the one everything else is measured
against. It renders pane contents with libghostty, the same emulator the
server uses, so what you see is what the server has.

Shipped in every release. [Getting started](./getting-started.md) is about
this client.

## Desktop

Phux Desktop is a native app (Rust, GPUI) for Apple silicon macOS 27+ and
Linux x86_64/arm64 with Vulkan. Install it with:

```sh
curl -fsSL https://phux.sh/install-desktop | sh
```

It shows every session on this machine and on each host you have
registered with `phux host add`, grouped by machine or by repository. One
window shows one session; the sidebar switches. Agents that need attention
are marked where they are. `⌘P` opens a palette over sessions, actions, and
settings. Updates are checked from the app and installed on restart.

It uses the installed `phux` CLI to start or reach the server, so install
the CLI first (the desktop installer does this if you have not). It never
creates sessions on its own and never stops the server; quitting the app
detaches.

Released alongside the CLI, with its own version number. The macOS build is
ad-hoc signed, not notarized. Expect rough edges; the terminal UI is ahead
of it. [`clients/desktop`](../clients/desktop) has the details.

## Browser

[`clients/phux-web`](../clients/phux-web) renders a session on a canvas,
with libghostty compiled to WebAssembly, over WebTransport or WebSocket.
It is a working client but not a shipped one: nothing serves it, and the
server's browser-facing listeners are off by default. Use it as the
starting point for embedding a terminal in a web page.

## Phones

iOS and Android apps are in progress in a separate repository, built on the
[`phux-client-ffi`](../crates/phux-client-ffi) bindings. They are not
available yet. `phux pair --qr` already prints the pairing code they will
scan.

## Your own

[Protocol](./protocol.md) describes the wire. The Rust crates
`phux-client-core` and `phux-client-runtime` do the client side for native
code, and `phux-client-ffi` exposes them to C, Swift, and Kotlin.
