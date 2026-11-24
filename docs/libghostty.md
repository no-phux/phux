---
group: internals
order: 4
---

# libghostty in phux

What the libghostty-vt engine does inside phux, what phux adds around it, and
what it deliberately does not do.

## What it does

The [pane actor](../crates/phux-terminal/src/actor/mod.rs) owns a
`libghostty_vt::Terminal`, the pane's PTY and the input encoders. `Terminal`
is `!Send`, so the actor runs on the server's current-thread `LocalSet`.

That terminal is the canonical screen. It parses the PTY bytes and delivers
OSC 7501 through `on_program_status` ([program protocol](./program-protocol.md)).
A rendering client feeds the same VT bytes to its own libghostty terminal and
reproduces the server's screen; input goes back as key, mouse, focus and paste
events in libghostty's vocabulary, which the server encodes for the PTY.
Selection and text extraction belong to libghostty.

The browser runs libghostty-vt compiled to WebAssembly:
[phux-vt-web](../clients/phux-vt-web/README.md) loads `ghostty-vt.wasm`, and
[phux-web](../clients/phux-web/README.md) paints its grid on a canvas.

Native and browser engines build one source revision.
`scripts/check-ghostty-pin.sh` fails unless the `libghostty-vt` revision in
`Cargo.lock`, `revision=` and `archive_sha256=` in
`scripts/build-vt-wasm.sh`, and the Zig version in
`.config/zig-toolchain.json` agree; bump them together. Build options are in
[development](./development.md#libghostty-vt).

## Snapshot compatibility

Native attach ships libghostty snapshots. Codec id `5` is the libghostty-vt
1.0 `GHOSTSNP` v1 snapshot, including Kitty images; id `4` is the pre-1.0
form, whose decoder cannot read the 1.0 image records. Current engines
advertise only `5`. Peers without a shared native codec negotiate synthesized
VT. The 1.x C ABI does not promise stable snapshot bytes.

- Capture is synchronous and scales with retained terminal state.
- One snapshot may retain at most 64 MiB, with separate limits on records,
  history units, rows and generations. Exceeding one fails capture before
  READY; it never silently drops history. The cap bounds retained output, not
  process memory: encoder scratch and image state are extra.
- History is encoded eagerly and delivered on demand after READY. All pages
  share one immutable allocation, so later scrollback pruning, clearing or
  reset cannot change captured bytes.

## What phux adds

- **Record framing and CRC32C preflight** in
  [`snapshot_framing.rs`](../crates/phux-protocol/src/snapshot_framing.rs),
  shared by native and browser replicas. Every record is checked before a
  history unit reaches the engine, so a corrupt suffix cannot partially apply.
- **PAGE lookahead.** With Kitty enabled, the stock decoder reads one more
  header after a PAGE. For a PAGE without placements, phux supplies a valid
  empty placement record locally; a bare PAGE followed by EOF poisons the
  decoder. Nothing extra goes on the wire.
- **Browser images.** Freestanding wasm has Kitty graphics disabled, so the
  browser adapter discards placements and pixels and reports them as zero-row
  progress.
- **Local Clear** cancels pending parser input with CAN, writes CUP/ED, then
  replays the saved continuation. If pending input exceeds the tracking limit,
  it returns a retry error without touching the display.

The engine fork adds only upstream-shaped pieces: a deferred-history clear
fence, effective input-option getters, an image iterator with placement
metadata, dirty-row and PNG-hook fixes, and a cursor fix for combined row
growth and column shrink. Do not add transport glue to the engine.

## Measuring capture

```sh
PHUX_CAPTURE_CHECK_ONLY=1 cargo bench -p phux-terminal --features capture-bench --bench capture
PHUX_CAPTURE_DHAT=1 cargo bench -p phux-terminal --features capture-bench --bench capture
```

Corpora and thresholds are in `crates/phux-terminal/benches/capture.rs`. To
time stock encoding and decoding against a separately built install prefix:

```sh
cc -O2 -I "$prefix/include" scripts/bench/libghostty-capture.c \
  "$prefix/lib/libghostty-vt.a" -o "$scratch/capture"
"$scratch/capture" 10000 10 1 0 11   # lines, history MiB, dense, 48-KiB images, samples
cc -O2 -I "$prefix/include" scripts/bench/libghostty-stock-decode.c \
  "$prefix/lib/libghostty-vt.a" -o "$scratch/decode"
"$scratch/decode"
```

## What it does not do

- It is not the multiplexer: sessions, tabs and processes belong to the
  server.
- It does not speak the wire. `phux-protocol` is pure Rust and compiles to
  wasm; libghostty glue lives in `phux-vt`.
- A client replica never answers terminal queries; the server's terminal is
  the only answerer, and the replica drops its replies
  ([engine adapter](../crates/phux-client-core/src/engine/ghostty.rs)).
- It does not paint chrome.
- It does not decide which agent occupies a pane; phux keeps that record.
- A text-only client runs no replica: it uses `GET_SCREEN`
  ([architecture](./architecture.md#bytes-out-events-in)).
