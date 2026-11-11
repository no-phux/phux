# libghostty in phux

libghostty-vt is the terminal engine inside phux: the server owns one
terminal per pane, and a rendering client runs its own. The
[phux README](../README.md) is the product overview. The engine's place
on the wire is the
[phux-protocol README](../crates/phux-protocol/README.md). The browser
client, which isn't shipped yet, is described in the
[phux-vt-web README](../clients/phux-vt-web/README.md) and the
[phux-web README](../clients/phux-web/README.md).

## What it does

The [pane actor](../crates/phux-terminal/src/actor/mod.rs)
owns a `libghostty_vt::Terminal`, the pane's pseudo-terminal, and the
input encoders. [Building the actor](../crates/phux-terminal/src/actor/construct.rs)
creates that terminal at the pane's size. `Terminal` is `!Send`, so the
actor runs on the server's current-thread `LocalSet`.

That terminal is the canonical screen. It parses the PTY byte stream.
OSC 7501 arrives through libghostty's `on_program_status` callback, and
phux stores the records
([program protocol](./program-protocol.md),
[effects](../crates/phux-terminal/src/actor/effects.rs)).

A rendering client feeds those VT bytes to its own libghostty terminal.
This is the Terminal kind's stream contract; other kinds declare their
own ([decisions](./decisions.md)). The
[phux-protocol README](../crates/phux-protocol/README.md) says a
libghostty terminal on the client reproduces the server's screen
exactly, and that a Terminal's client-to-server input is key, mouse,
focus, and paste events built from libghostty's vocabulary. The server encodes
those events for the PTY. Selection and text extraction belong to
libghostty ([decisions](./decisions.md)).

The browser client, which isn't shipped yet, draws with libghostty-vt compiled to WebAssembly.
[phux-vt-web](../clients/phux-vt-web/README.md) loads `ghostty-vt.wasm`,
writes VT bytes, and reads the styled grid.
[phux-web](../clients/phux-web/README.md) paints that grid on a canvas.
Embedding runs this replica in the app's process
([embed phux](./embedding.md)).

Builds use a pinned Ghostty commit, compiling it from source with Zig when
no matching prebuilt `libghostty-vt.a` is published
([development](./development.md#libghostty-vt)).

## Upstream correctness backports

The shipping engine keeps its existing snapshot contract and Zig toolchain.
Native and browser engines include the same upstream fixes without switching
to the development libghostty-vt 1.1 interface:

- [Partial resize backfill](https://github.com/ghostty-org/ghostty/commit/1b8b3f55bcc93d2685f7771948516af1222dcc46):
  remap references to copied rows before destroying their old page, even
  when a later row does not fit. Release the failed row's partial allocation.
- [Combined row growth and column shrink](https://github.com/ghostty-org/ghostty/commit/321a32a42393ab12721d2308ffff8957ce6853ad):
  adjust the cursor's active-area coordinates before reflow so scrollback
  pulled into view is not pushed back out. Resolve the coordinate through a
  tracked cursor reference so pruning an old page during row growth is not
  mistaken for cursor movement.
- [Render allocation recovery](https://github.com/ghostty-org/ghostty/commit/3e29fcd628a50757ad24988c721340e56f392485):
  keep failed and unprocessed rows dirty until rebuilding succeeds, so a
  later update retries rather than exposing partially initialized cells.

The native and WASM resize regressions both failed on the previous pins:
growing a 5x2 terminal to 4x4 showed only `3C` instead of restoring
`1A`, `2B`, and `3C`. Tests also check that subsequent output lands on the
correct cursor row. Upstream engine tests cover partial backfill and render
allocation failure.
An additional engine regression forces row growth to evict history at the
byte limit and checks cursor preservation with and without a caller-supplied
tracked reference.
The browser retains its existing 32-bit-safe terminal-identity implementation;
native and WASM source revisions therefore differ, but the three corrected
terminal modules are identical.

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
