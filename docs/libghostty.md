# libghostty in phux

libghostty-vt is the terminal engine inside phux: the server owns one
terminal per pane, and a rendering client runs its own. The
[phux README](../README.md) is the product overview. The engine's place
on the wire is the
[phux-protocol README](../crates/phux-protocol/README.md). The browser
build is the [phux-vt-web README](../clients/phux-vt-web/README.md) and
the [phux-web README](../clients/phux-web/README.md).

## What it does

The [pane actor](../crates/phux-server/src/resource/terminal/mod.rs)
owns a `libghostty_vt::Terminal`, the pane's pseudo-terminal, and the
input encoders. [Building the actor](../crates/phux-server/src/resource/terminal/construct.rs)
creates that terminal at the pane's size. `Terminal` is `!Send`, so the
actor runs on the server's current-thread `LocalSet`.

That terminal is the canonical screen. It parses the PTY byte stream.
OSC 7501 arrives through libghostty's `on_program_status` callback, and
phux stores the records
([program protocol](./program-protocol.md),
[effects](../crates/phux-server/src/resource/terminal/effects.rs)).

A rendering client feeds those VT bytes to its own libghostty terminal.
The [phux-protocol README](../crates/phux-protocol/README.md) says a
libghostty terminal on the client reproduces the server's screen
exactly, and that client-to-server input is key, mouse, focus, and
paste events built from libghostty's vocabulary. The server encodes
those events for the PTY. Selection and text extraction belong to
libghostty ([decisions](./decisions.md)).

The browser client draws with libghostty-vt compiled to WebAssembly.
[phux-vt-web](../clients/phux-vt-web/README.md) loads `ghostty-vt.wasm`,
writes VT bytes, and reads the styled grid.
[phux-web](../clients/phux-web/README.md) paints that grid on a canvas.
Embedding runs this replica in the app's process
([embed phux](./embedding.md)).

Builds download a prebuilt `libghostty-vt.a` for a pinned Ghostty
commit, or compile it from source with Zig
([development](./development.md#libghostty-vt)).

## What it does not do

libghostty is not the multiplexer. Sessions, tabs, and the process in
the pane belong to the server. The library holds one pane's terminal.

It does not speak the server protocol. The default `phux-protocol`
build is pure Rust, with no libghostty, and compiles to wasm. The
`server` feature adds libghostty conversions; decoding wire messages
does not need them
([phux-protocol README](../crates/phux-protocol/README.md)).

The WebAssembly driver does no networking, does not speak the phux
protocol, and does not touch the DOM
([phux-vt-web README](../clients/phux-vt-web/README.md)).

A client replica does not answer terminal queries. The server's
canonical terminal is the only query answerer, and the replica drops
the replies it would generate
([engine adapter](../crates/phux-client-core/src/engine/ghostty.rs),
[terminal queries](./protocol.md#terminal-queries)).

It does not paint chrome. Pane interiors are libghostty's; the client's
chrome is separate ([decisions](./decisions.md)).

A client that only wants text does not run a replica. It asks the
server for `GET_SCREEN` and gets the server's view, which is what
`phux snapshot` does
([architecture](./architecture.md#bytes-out-events-in)). An attach can
instead use synthesized VT, which works with any emulator
([attach and output](./protocol.md#attach-and-output)).

libghostty does not decide which coding agent occupies a pane. phux
keeps that record itself
([agent record](./program-sequences.md#the-agent-record)).
