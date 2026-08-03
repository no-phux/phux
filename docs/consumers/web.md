---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-07-10
---

# The phux web client

**TL;DR.** phux-web is the reference projection consumer: a browser client,
written in Rust to WASM, that carries its own terminal engine. It loads
ghostty-vt.wasm, speaks the exact phux-protocol wire codec over WebTransport
(HTTP/3 over QUIC — tried first when a `?wt=` URL is given, falling back to
WebSocket) or a WebSocket, and computes its rendered view from engine state
it owns rather than reading structured state off the wire. It paints one
terminal to a canvas and routes keystrokes back as input atoms.
Single-terminal today; splits and layout are out of scope.

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
| `phux-web` | Rust | The client: a WebSocket `Session` over the engine, a `<canvas>` renderer (cursor blink), keyboard handling, and the `#[wasm_bindgen]` `start()` entry. |

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
3. Connect to the server. Two paths, same wire:
   - **WebTransport** (`phux server --webtransport` / `PHUX_WT_ADDR`): an
     HTTP/3-over-QUIC session whose single bidirectional stream carries
     length-prefixed frames, exactly the Unix-socket framing; chunks are
     reassembled client-side. Tried first when a `?wt=<https-url>` query
     parameter (or the `start_webtransport` entry point) supplies the
     session URL; on a token-authenticated listener the pairing token rides
     the URL as `?token=<hex>` because the JS `WebTransport` API cannot set
     request headers.
   - **WebSocket** (`PHUX_WS_ADDR`): one binary message carries one encoded
     frame. The fallback (and the default when no WebTransport URL is
     given).
4. Send protocol-0.7 `HELLO`, consume its selected bootstrap profile/bounds,
   then send `ATTACH` with a client correlation `attach_id`.
5. Decode matching `BOOTSTRAP_BEGIN` / `BOOTSTRAP_CHUNK` into an invisible
   engine and publish at `BOOTSTRAP_READY`; consume generation-bound
   `TERMINAL_OUTPUT` from `base_seq + 1`. Native checkpoint and raw live bytes
   are never rewritten. Raw profiles send no `FRAME_ACK`; only negotiated
   `SynthesizedVtStateSync` acknowledges a transition after applying it.
   Retained history is requested incrementally after READY. A 530 ms interval
   toggles cursor blink.
6. On `keydown`, map `KeyboardEvent.code` to a `PhysicalKey`, build a
   `KeyEvent`, and send `INPUT_KEY` for the attached terminal.

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
| Zig | 0.16.x | nix devshell (`zig_0_16`) | building `ghostty-vt.wasm` from ghostty |
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

For the WebTransport path, run a real server with a WebTransport listener and
hand the client both URLs (it tries WebTransport, then falls back):

```sh
phux server --webtransport 127.0.0.1:4433 --listen 127.0.0.1:8787
# then: start_webtransport("https://127.0.0.1:4433/session",
#                          "ws://127.0.0.1:8787/", canvasId, cols, rows)
# or load the Trunk page with ?wt=https%3A%2F%2F127.0.0.1%3A4433%2Fsession
```

Browsers require a certificate they trust for WebTransport (a plain
self-signed cert is rejected unless registered via
`serverCertificateHashes`, a follow-up for the pairing flow); local Chrome
can be launched with `--origin-to-force-quic-on` +
`--ignore-certificate-errors-spki-list` for development.

## Scope and limits

- **Single terminal.** No splits, windows, or layout chrome — that is the TUI's
  job. The web client mirrors one terminal.
- **Text, color, cursor.** The canvas renderer paints grapheme cells with fg/bg
  and a blinking block cursor. Images and sixel (which the engine does parse)
  are a future renderer pass. Accordingly, the client's `HELLO` advertises **no
  image protocols** (`Session::client_caps`), so the server strips kitty
  graphics, sixel, and iTerm2 image escapes before forwarding (SPEC 6.2,
  ADR-0034) instead of shipping payloads the canvas would drop. When the
  renderer pass lands, the advertisement widens with it.
- **Engine boundary copies.** Bytes cross two wasm linear memories (the Rust
  client and `ghostty-vt.wasm`), which is fine for terminal traffic.

## Verification

- `phux-vt-web` — `wasm-pack test --node`: drives the real engine, reads the
  grid back, decodes a truecolor cell and the cursor.
- `phux-web` — `wasm-pack test --node`: generation-bound
  `TERMINAL_OUTPUT` round-trips through the codec and feeds the engine; native
  and synthesized-raw profiles do not ACK.
- Renderer and full client — `wasm-pack test --headless --chrome`:
  engine-to-grid-to-canvas pixel test, and a live connect-to-server-and-render
  end-to-end test against `ws_demo_server`.
- Server side — `phux-server` attach tests: a real protocol-0.7 client receives
  `ATTACHED`, per-pane BEGIN/CHUNK/READY, and aggregate `ATTACH_READY`.
- WebTransport listener — `phux-server` `transport::webtransport` tests: a
  native WebTransport client (wtransport) performs the full HTTP/3 session
  handshake against the listener, exercises the bearer-token gate (header
  and `?token=` carriers accepted; missing/invalid refused before the
  session exists), and round-trips frames both directions over the bidi
  stream. The client-side frame reassembly (`framing::FrameBuffer`) is
  covered by `wasm-pack test --node`.
