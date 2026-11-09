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
The [phux-protocol README](../crates/phux-protocol/README.md) says a
libghostty terminal on the client reproduces the server's screen
exactly, and that client-to-server input is key, mouse, focus, and
paste events built from libghostty's vocabulary. The server encodes
those events for the PTY. Selection and text extraction belong to
libghostty ([decisions](./decisions.md)).

The browser client, which isn't shipped yet, draws with libghostty-vt compiled to WebAssembly.
[phux-vt-web](../clients/phux-vt-web/README.md) loads `ghostty-vt.wasm`,
writes VT bytes, and reads the styled grid.
[phux-web](../clients/phux-web/README.md) paints that grid on a canvas.
Embedding runs this replica in the app's process
([embed phux](./embedding.md)).

The engine is based on [libghostty-vt 1.0](https://lib.ghostty.org/vt/1.0.0/).
The Rust bindings track upstream, with phux extensions for staged decoding,
input-option snapshots, parser-safe display clearing, image replay metadata,
and safe formatter/PNG integration. Capture uses the stock snapshot encoder;
phux bounds and indexes the resulting immutable bytes.
Upstream terminal semantics and image placement resolution remain authoritative.

The new pin builds from source with Zig 0.17 and shares the resulting native
build across worktrees. No matching native prebuilt archives have been published;
see [development](./development.md#libghostty-vt).

## Snapshot compatibility

libghostty-vt 1.0 snapshots use native codec id `5`; pre-1.0 snapshots with
empty READY/FINISH markers use id `4`. Both have a `GHOSTSNP` version-1
envelope, but the older decoder cannot read the newer Kitty image records.
Current engines advertise only id `5`. Mixed old/new peers negotiate
synthesized VT instead; native-only peers refuse the incompatible contract.
libghostty's stable 1.x C ABI does not guarantee stable snapshot bytes.

Capture is synchronous and its work scales with retained terminal state.
The source is unchanged until stock encoding returns. The encoder writes to
a capped sink: one snapshot may retain at most 64 MiB, and records, history
units, rows, and generation counts have separate limits. Exceeding a limit
fails capture explicitly; it never silently drops history.

This is a retained-output limit, not a total capture-memory limit. Stock
encoding buffers and checksums a complete record before calling the sink.
Its temporary record scratch and image plan are additional memory; source
scrollback limits and the 64 MiB image-storage policy bound that state. An
oversized record can therefore allocate scratch before indexing refuses it.
If the full snapshot exceeds a limit, native attach now fails before READY,
rather than publishing the screen and failing a later history request.
The genuine over-cap image refusal/recovery test peaked at about 141 MiB
process RSS on the measurement host; a 64 MiB retained-output cap must not be
presented as a 64 MiB process-memory guarantee.

History is encoded eagerly but delivered on demand after READY. All pages
share one immutable allocation. A queued page retains the reservation for
that entire allocation, even after its generation is released. Source
scrollback pruning, clearing, or reset cannot change captured bytes. Resize,
reattach, expiry, and explicit release still follow the generation lifecycle.

## Why patches remain

Exact attach, progressive history, parser continuation, and live output
between history decode calls are all upstream capabilities. They do not
justify a custom capture engine. The former detached-capture implementation
and its page-mutation fences have been removed rather than moved into phux.

The remaining extensions have narrower purposes:

| Extension | Reason |
| --- | --- |
| Staged history decoding | A stock PAGE decode reads the next record header to discover optional Kitty placements. phux units end at the PAGE/placement boundary. Keeping this small extension avoids changing the wire or adding prefetch/accounting machinery. |
| Whole-unit CRC preflight | Checks every record, including placements and unknown optional records, before applying the unit. This does not promise transactions for semantically invalid, CRC-valid data. |
| Deferred-history clear fence | A live scrollback clear must discard pre-clear pages still arriving after READY, without dropping deferred pixels for surviving active-screen images. |
| Input-option snapshots | Publishes the engine's effective key and mouse options to the independent input lane without duplicating mode precedence. |
| Parser-preserving Clear | Clears the local presentation without consuming unfinished CSI, OSC, DCS, or UTF-8 input. Injecting clear escape sequences is not equivalent. |
| Image metadata | Preserves unplaced images and placeholder identity/crops in synthesized replay and rendering. Native snapshots themselves already preserve images upstream. |
| Rendering/PNG fixes | Correct dirty-row tracking and synchronize the process-global PNG hook used by concurrent embedded terminals. |

Formatter convenience wrappers and build caching are integration choices,
not evidence that the engine must be forked. An application-owned Zig shim
could expose some missing operations, but cannot safely unwrap an existing
opaque C terminal through the public interface. Replacing terminal ownership
just to relocate these wrappers would be a larger change.
Likewise, staged decoding is a transport integration choice, not an upstream
inability to load history progressively. These patches are the smaller
current integration, not a claim that every helper intrinsically requires
a fork. A startup-only PNG hook policy may also avoid the atomic-hook patch;
that requires controlling every initializer in the embedding process.

[Rex's author commits to using the public MIT-licensed building blocks](https://mitchellh.com/writing/superlogical),
and [reported fixes found during Rex's own 1.0 upgrade](https://github.com/ghostty-org/ghostty/pull/15134).
Rex's terminal replication implementation is not public. It is not evidence
for a particular capture or history algorithm. The relevant public contract
is the [upstream snapshot guide](https://lib.ghostty.org/vt/1.0.0/guide-snapshots.html).

## Capture measurements

Measured on an Apple M4 Pro on November 8, 2026, using stock release
`0fa3d577a5614150ff8e2c152de5fb9d5c6d2dec`, Zig 0.17.0,
`ReleaseFast`, baseline CPU, and the median of ten warm samples:

| Terminal state, 120x40 | Full stock encoding |
| --- | ---: |
| Default 10 MiB history budget, dense text | 0.48 ms |
| 100,000 dense rows, 128 MiB history budget | 5.14 ms |
| 1,000,000 input rows, pruned to 128 MiB | 6.51 ms |
| Default text history plus 48 MiB of Kitty images | 6.30 ms |

These are engine-only timings into a discard writer, not end-to-end attach
latency or a guarantee on slower hardware. Snapshot buffering and indexing
add work. The previous detached capture released the source in roughly
7–149 microseconds for these workloads, but required a separate resumable
encoder and mutation hooks throughout page storage. The simpler path trades
that history-independent capture cost for bounded, eager encoded storage.

The actual Rust buffering/indexing benchmark measured about 9.28 ms p95 for
the wider Unicode 50,000-line corpus (18.3 MB encoded). Its Rust allocation
probe peaked at 22.5 MB; this excludes the native engine allocator. Small
shell and TUI captures measured about 2 and 29 microseconds p95 respectively.
Reproduce the gate and Rust allocation probe with:

```sh
PHUX_CAPTURE_CHECK_ONLY=1 cargo bench -p phux-terminal --features capture-bench --bench capture
PHUX_CAPTURE_DHAT=1 cargo bench -p phux-terminal --features capture-bench --bench capture
```

The fixed corpora and thresholds live in
`benchmarks/support.rs` and `crates/phux-terminal/benches/capture.rs`.

The real-server warm-history release gate also passed with eight native
owners, one stalled reader, independent history paging, and exact-once live
output. One local run reached READY for all eight in 5.90 ms. A separate
probe issued established-client echoes concurrently from the attach barrier,
before any BEGIN, and observed a slowest echo of 1.72 ms. These are local
observations, not a latency guarantee or percentile estimate.

To reproduce against an independently built stock install prefix:

```sh
cc -O2 -Wall -Wextra -Werror -I "$prefix/include" \
  scripts/bench/libghostty-capture.c "$prefix/lib/libghostty-vt.a" -o "$scratch/capture"
"$scratch/capture" 10000 10 1 0 11
# Arguments: input lines, history MiB, dense (0/1), 48-KiB image count, samples.
"$scratch/capture" 100000 128 1 0 11
"$scratch/capture" 10000 10 0 1024 11
cc -O2 -Wall -Wextra -Werror -I "$prefix/include" \
  scripts/bench/libghostty-stock-decode.c "$prefix/lib/libghostty-vt.a" -o "$scratch/decode"
"$scratch/decode"
```

The decoder probe exercises READY, unfinished CSI continuation, live input,
deferred pages, and FINISH using only the stock C interface. It also proves
that a bare PAGE followed by temporary EOF poisons subsequent decoding;
adding bytes later does not repair it. Real lookahead is necessary.
The optional `PHUX_CAPTURE` benchmark build compares the historical detached
capture pin `0aa049ea7b9075edbd1b598f03290f278ab8a4cc`; it is not a current
interface.

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
