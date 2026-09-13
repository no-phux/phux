# phux-web

The **phux browser client**, compiled to WebAssembly. It renders a live phux
terminal in a `<canvas>` using the *real* `libghostty-vt` engine and the *real*
phux wire codec — no JavaScript terminal, no reimplementation. A peer of the
reference TUI and the agent SDK ([ADR-0017]); same wire, different projection.

See [`docs/consumers/web.md`](../../docs/consumers/web.md) for the full
architecture; this is the crate-level summary.

## What it glues together

```text
phux-vt-web   ── drives ghostty-vt.wasm (the VT engine)          "render bytes"
phux-protocol ── the exact FrameKind wire codec (shared w/ server)  "the wire"
web-sys       ── WebSocket, <canvas>, KeyboardEvent                 "the browser"
        └────────────────► phux-web ◄────────────────┘
```

`phux-web` connects to a phux server — over WebTransport (HTTP/3 over QUIC,
via `phux server --webtransport`; the browser's QUIC-class transport) when a
session URL is supplied, falling back to a WebSocket — decodes each frame
with `phux-protocol`, feeds the terminal bytes into the engine via
`phux-vt-web`, paints the grid (with a blinking cursor), and sends keystrokes
back as `INPUT_KEY` frames. Both transports carry the identical wire; the
WebTransport stream is length-prefixed frames reassembled by
`framing::FrameBuffer`, a WebSocket message is one frame.

## The dependency chain (build time)

The confusing part: there are **two wasm modules, one nested in the other.**

```text
ghostty (Zig)
   │  zig build               (scripts/build-vt-wasm.sh)
   ▼
ghostty-vt.wasm  ── the engine, vendored into phux-vt-web/vendor/
   │  include_bytes!          (baked in as raw bytes)
   ▼
phux-vt-web ──┐
              ├──►  phux-web  ──wasm-pack build──►  phux_web_bg.wasm + phux_web.js
phux-protocol ┤
web-sys ──────┘
```

`phux_web_bg.wasm` (this crate, Rust) **literally contains** `ghostty-vt.wasm`
(the engine, Zig) as embedded bytes. At runtime the Rust module instantiates the
Zig module as a **second, separate** wasm instance and calls across to it. Two
wasm instances live in the page; bytes are copied across the boundary (fine for
terminal traffic). See [ADR-0024]/[ADR-0025] for why this beats linking them.

## Runtime flow

```text
1. browser loads phux_web.js + phux_web_bg.wasm
2. Rust client boots → instantiates the embedded ghostty-vt.wasm
3. opens a WebSocket to the phux server
4. server ──► binary frame (FrameKind)  → decode with phux-protocol → VT bytes
5. write VT bytes into the engine (vt_write) → engine updates its grid + cursor
6. read the grid back → paint <canvas>; a 530 ms interval blinks the cursor
7. you type ──► KeyboardEvent → phux-protocol INPUT_KEY → WebSocket → server
```

## Public API

Two `#[wasm_bindgen]` entry points, designed to be driven from JS:

```js
import init, { start, start_webtransport } from "./pkg/phux_web.js";
await init();
// finds <canvas id="…">, connects, attaches, and runs for the connection's life
await start("wss://host/session", "my-canvas", /*cols*/ 100, /*rows*/ 24);
// or WebTransport-first (phux server --webtransport), WebSocket fallback;
// on a token-authenticated listener append ?token=<hex> to the https URL:
await start_webtransport("https://host:4433/session", "wss://host/session",
                         "my-canvas", 100, 24);
```

## Building

Follow [Contributor setup](../../docs/SETUP.md#browser-client) for native or Nix
tools. The engine module is committed, so ordinary client work needs only the
Rust/WASM tools:

```sh
# build this client to a web package using the committed engine:
cd clients/phux-web && wasm-pack build --target web --release --out-dir pkg
#    → pkg/phux_web.js + pkg/phux_web_bg.wasm  (~6 MB; engine included)
```

Tool versions and installation commands live in the setup guide;
`bash scripts/doctor.sh web` checks them from the repository root.
Changing the engine itself additionally needs Zig; the setup guide documents
verified source acquisition and the byte-for-byte regeneration check.

## Tests

```sh
wasm-pack test --node                 # session/codec → engine → ack
wasm-pack test --headless --chrome    # render + live connect-to-server e2e
```

## Scope

Single terminal; text + color + cursor. Splits/layout are the TUI's job. Image
drawing (sixel/Kitty graphics — which the engine *parses*) is a future renderer
pass.

[ADR-0017]: ../../docs/adr/0017-tui-not-protocol-privileged.md
[ADR-0024]: ../../docs/adr/0024-wire-owns-input-atoms.md
[ADR-0025]: ../../docs/adr/0025-browser-web-client.md

## License

MIT OR Apache-2.0.
