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

`phux-web` opens a WebSocket to a phux server, decodes each binary frame with
`phux-protocol`, feeds the terminal bytes into the engine via `phux-vt-web`,
paints the grid (with a blinking cursor), and sends keystrokes back as
`INPUT_KEY` frames.

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

Two `#[wasm_bindgen]` entry points are designed to be driven from JS. The
original `start()` API continues to connect directly to a phux endpoint:

```js
import init, { start } from "./pkg/phux_web.js";
await init();
// finds <canvas id="…">, connects, attaches, and runs for the connection's life
await start("wss://host/session", "my-canvas", /*cols*/ 100, /*rows*/ 24);
```

Hosted endpoints use `start_hosted()`. They must send one text control message
before any binary wire frame; every later message must be binary. The callback
receives only validated lifecycle data:

```js
import init, { start_hosted } from "./pkg/phux_web.js";
await init();

const session = await start_hosted(
  "wss://host/session?mode=native",
  "my-canvas",
  100,
  24,
  (event) => {
    // event is HostedSessionEvent below; no WebSocket close reason is exposed.
  },
);

// Idempotent and synchronous: closes the socket, detaches all handlers,
// clears cursor blinking, and suppresses later lifecycle callbacks.
session.close();
```

The exact JavaScript/TypeScript signature is:

```ts
function start_hosted(
  ws_url: string,
  canvas_id: string,
  cols: number,
  rows: number,
  callback: Function,
): Promise<HostedClient>;

class HostedClient {
  close(): void;
}
```

```ts
type HostedSessionEvent =
  | {
      type: "phux.session.v1";
      outcome: "accepted";
      backend: "native" | "edge";
      expiresAt: number;
      fallbackReason?:
        | "auth-required"
        | "account-concurrency"
        | "hourly-quota"
        | "daily-quota"
        | "native-capacity"
        | "ip-capacity"
        | "native-disabled"
        | "native-unhealthy"
        | "startup-timeout"
        | "startup-failed";
    }
  | { type: "error"; category: "transport" | "protocol" | "client" }
  | {
      type: "close";
      code: number;
      category:
        | "normal"
        | "going-away"
        | "protocol"
        | "server"
        | "unavailable"
        | "capacity"
        | "rate-limited"
        | "bad-request"
        | "idle"
        | "expired"
        | "unauthorized"
        | "network";
      wasClean: boolean;
    };
```

Malformed, duplicate, or later text controls and binary-before-control close
with protocol error. Unknown close codes normalize to code `1006` and category
`"network"`; arbitrary peer close reasons never enter the callback. Before the
engine is ready, at most 64 binary frames totaling at most 1 MiB are buffered;
exceeding either limit is a protocol error. Keyboard input is attached to the
focusable canvas, not the document. `close()` is safe to call repeatedly. A
peer close also detaches keyboard handling and clears the blink interval.

## Building

Two steps, because of the two modules. Run inside the phux nix devshell (it
provides the whole toolchain):

```sh
# 1. build the engine artifact (once; regenerate when ghostty bumps):
scripts/build-vt-wasm.sh

# 2. build this client to a web package:
cd clients/phux-web && wasm-pack build --target web --release --out-dir pkg
#    → pkg/phux_web.js + pkg/phux_web_bg.wasm  (~6 MB; engine included)
```

| Build input | Version | For |
|---|---|---|
| Rust | 1.90 | the client (target `wasm32-unknown-unknown`) |
| Zig | 0.15.x | the engine artifact (`ghostty-vt.wasm`) |
| `wasm-pack` | 0.15 | packaging |
| `wasm-bindgen` / `-cli` | `=0.2.121` | bindings (crate pin **must** match the CLI) |
| `binaryen` (`wasm-opt`) | — | release size optimization |
| `chromedriver` + Chrome | matched | `--headless --chrome` tests only |

## Tests

```sh
wasm-pack test --node                 # session/codec → engine → ack
wasm-pack test --headless --chrome    # render + live connect-to-server e2e
```

## Scope

Single terminal; text + color + cursor. Splits/layout are the TUI's job. Image
drawing (sixel/Kitty graphics — which the engine *parses*) is a future renderer
pass.

[ADR-0017]: ../../ADR/0017-tui-not-protocol-privileged.md
[ADR-0024]: ../../ADR/0024-wire-owns-input-atoms.md
[ADR-0025]: ../../ADR/0025-browser-web-client.md

## License

MIT OR Apache-2.0.
