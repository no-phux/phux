---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-06-06
---

# The phux web client

**TL;DR.** phux-web is the reference projection consumer: a browser client,
written in Rust to WASM, that carries its own terminal engine. It loads
ghostty-vt.wasm, speaks the exact phux-protocol wire codec over a WebSocket,
and computes its rendered view from engine state it owns rather than reading
structured state off the wire. It paints one terminal to a canvas and routes
keystrokes back as input atoms. Single-terminal today; splits and layout are
out of scope.

---

## What it is, and why it is the reference

phux-web is a peer consumer alongside the reference TUI and the agent client
([ADR-0017](../../ADR/0017-tui-not-protocol-privileged.md)): same wire,
different projection. The TUI projects the terminal to VT bytes on a real tty;
the web client projects it to a canvas grid in a browser.

[ADR-0030](../../ADR/0030-engine-delegated-wire-and-projection-consumers.md) §4
names phux-web the **reference pattern** for any consumer that wants structured
terminal state: **carry your own engine and project locally.** The wire carries
opaque terminal bytes, not a structured screen model; phux-web runs libghostty
in the browser, feeds it the bytes off the wire, and reads its grid back. An
agent SDK that wants structure should copy this shape rather than expect a
structured wire tier. phux-web is the concrete, shipping proof that the
projection thesis works: the engine is shared, never re-encoded, so there is no
second terminal model on the wire to drift.

This is the design from [ADR-0025](../../ADR/0025-browser-web-client.md),
realized in code and verified end-to-end in headless Chrome.

Two crates make it up, plus one vendored artifact:

| Piece (`clients/`) | Language | Role |
|---|---|---|
| `ghostty-vt.wasm` | Zig (ghostty's) | The VT engine itself: parses escape codes, holds the grid and cursor. A vendored build artifact, not phux code. |
| `phux-vt-web` | Rust | A safe driver over the engine's C ABI: make a `Terminal`, `write` VT bytes, read a styled `Grid` (cells, fg/bg, cursor). Depends on nothing phux. |
| `phux-web` | Rust | The client: a WebSocket `Session` over the engine, a `<canvas>` renderer (cursor blink), keyboard handling, and the `#[wasm_bindgen]` `start()` / hosted `start_hosted()` entries. |

## The two-wasm architecture

There are two wasm modules, one nested inside the other. The engine module is
self-contained — its only import is `env.log`, and it ships its own allocator —
so `phux-vt-web` loads it through the plain `WebAssembly` JS API, with no Zig
linked into the Rust wasm binary. Linking them would mean sharing one wasm
linear memory between two toolchains; instead the Rust module runs the engine
as a sibling instance and copies bytes across the boundary
([ADR-0025](../../ADR/0025-browser-web-client.md)).

```text
phux_web_bg.wasm  (Rust client)
   |- embeds, then instantiates --> ghostty-vt.wasm  (Zig engine)
        two wasm instances live in the tab; bytes cross the boundary
```

## Dependency chain (build time)

```text
ghostty (Zig source)
   |  zig build -Demit-lib-vt        (scripts/build-vt-wasm.sh)
   v
ghostty-vt.wasm  -- vendored into clients/phux-vt-web/vendor/
   |  include_bytes!                 (baked into the Rust binary)
   v
phux-vt-web --+
              +--> phux-web  --wasm-pack build--> phux_web_bg.wasm + phux_web.js
phux-protocol +                                          |
web-sys ------+                                          v  (consumed by phux-site)
                                              <import init, { start }>
```

`phux-protocol` is the same wire codec the server uses. It became wasm-safe in
[ADR-0024](../../ADR/0024-wire-owns-input-atoms.md): the wire owns its input
atoms, so the codec no longer pulls in libghostty on the client. The codec is
the one documented in [`../spec/appendix-encoding.md`](../spec/appendix-encoding.md).

## Runtime flow

1. Browser loads `phux_web.js` (glue) and `phux_web_bg.wasm` (the Rust client).
2. The client boots and instantiates the embedded `ghostty-vt.wasm` (the engine).
3. Open a WebSocket to the server (`PHUX_WS_ADDR`); one binary message carries
   one encoded frame — the same wire the Unix-socket transport uses.
4. Send `HELLO`, then `ATTACH` (`CreateIfMissing` the `default` session).
5. On `TERMINAL_SNAPSHOT` / `TERMINAL_OUTPUT`, write the bytes into the engine
   and repaint the grid; ack output with `FRAME_ACK`. A 530 ms interval toggles
   the cursor blink.
6. On `keydown`, map `KeyboardEvent.code` to a `PhysicalKey`, build a
   `KeyEvent`, and send `INPUT_KEY` for the attached terminal.

### Hosted session control

`start_hosted(wsUrl, canvasId, cols, rows, callback)` is the hosted-only API and
returns `Promise<HostedClient>`, where `HostedClient.close(): void` is
idempotent. `close()` synchronously suppresses subsequent callbacks, closes the
socket, removes its handlers and the canvas-scoped keyboard listener, and
clears cursor blinking. A peer close performs the listener/timer cleanup
automatically. The canvas is made focusable while the client is active.
Its socket handlers are installed before the embedded engine is loaded, so an
immediate first message or close cannot race startup. Accepted binary messages
are queued until the engine is ready, capped at 64 frames and 1 MiB total;
exceeding either cap is a protocol error. After acceptance their wire decode,
render, acknowledgement, and keyboard paths are the same as `start()`.

The hosted endpoint must send exactly one first text message:

```json
{"type":"phux.session.v1","outcome":"accepted","backend":"native","expiresAt":1786800000000}
```

`backend` is `"native"` or `"edge"`. Edge fallback responses may additionally
carry a bounded `fallbackReason` from the deployed vocabulary documented in the
crate README. Binary-before-control, malformed or duplicate control, any later
text, unknown fields, and arbitrary fallback strings fail closed with WebSocket
protocol error `1002`.

The callback receives the validated accepted object, normalized `error` events,
and exactly one normalized `close` event. Close events contain `code`,
`category`, and `wasClean`; they never contain the peer's arbitrary close
reason. Known hosted private codes map to capacity (`4001`), rate limit (`4002`),
bad request (`4003`), idle (`4004`), expiry (`4005`), unauthorized (`4007`), and
server error (`4011`). Unknown codes become `1006` / `"network"`.

The render path reads the grid out of the engine the client runs — there is no
structured screen state on the wire to consume. That is the projection
pattern, concretely.

## Building

Two steps, because of the two modules. Run inside the phux nix devshell, which
provides the entire toolchain (`nix develop`):

```sh
# 1. Build the engine artifact (once; regenerate when ghostty bumps). Needs zig.
scripts/build-vt-wasm.sh
#    -> clients/phux-vt-web/vendor/ghostty-vt.wasm   (gitignored)

# 2. Build the client to a web package.
cd clients/phux-web && wasm-pack build --target web --release --out-dir pkg
#    -> pkg/phux_web.js + pkg/phux_web_bg.wasm  (~6 MB; engine included)
```

`build.rs` in `phux-vt-web` fails with a clear message if step 1 hasn't run.

### Build dependencies

| Tool | Version | Provided by | For |
|---|---|---|---|
| Rust | 1.90.0 | `rust-toolchain.toml` (targets `wasm32-unknown-unknown`) | both crates |
| Zig | 0.15.x | nix devshell (`zig_0_15`) | building `ghostty-vt.wasm` from ghostty |
| `wasm-pack` | 0.15 | nix devshell | packaging the client |
| `wasm-bindgen` / `wasm-bindgen-cli` | `=0.2.121` | crate pin + nix devshell | bindings — the crate pin must equal the CLI version |
| `binaryen` (`wasm-opt`) | nixpkgs | nix devshell | release size optimization |
| `chromedriver` + Chrome | matched majors | nix devshell + system Chrome | `--headless --chrome` tests only |

The ghostty checkout should be pinned to the same revision `libghostty-vt-sys`
uses (see the native crates' `Cargo.toml`), so the browser engine matches the
one native phux links.

## Running it locally

```sh
# A standalone seeded server to point a build at:
PHUX_WS_ADDR=127.0.0.1:47654 cargo run -p phux-server --example ws_demo_server
```

Then serve the `pkg/` output and call `start("ws://127.0.0.1:47654/", canvasId,
cols, rows)`. The phux-site repo wires this into a `<PhuxTerminal>` island; see
its `scripts/build-client.sh` for the copy-the-artifact step.

## Scope and limits

- **Single terminal.** No splits, windows, or layout chrome — that is the TUI's
  job. The web client mirrors one terminal.
- **Text, color, cursor.** The canvas renderer paints grapheme cells with fg/bg
  and a blinking block cursor. Images and sixel (which the engine does parse)
  are a future renderer pass.
- **Engine boundary copies.** Bytes cross two wasm linear memories (the Rust
  client and `ghostty-vt.wasm`), which is fine for terminal traffic.

## Verification

- `phux-vt-web` — `wasm-pack test --node`: drives the real engine, reads the
  grid back, decodes a truecolor cell and the cursor.
- `phux-web` — `wasm-pack test --node`: a real `TERMINAL_OUTPUT` frame
  (round-tripped through the codec) feeds the engine and acks.
- Renderer and full client — `wasm-pack test --headless --chrome`:
  engine-to-grid-to-canvas pixel test, and a live connect-to-server-and-render
  end-to-end test against `ws_demo_server`.
- Server side — `phux-server` `ws_attach` test: a real client does
  `HELLO`-then-`ATTACH` over WebSocket and receives `ATTACHED` plus
  `TERMINAL_SNAPSHOT`.
