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
session URL is supplied, falling back to a WebSocket after a bounded setup
and protocol HELLO — decodes each frame
with `phux-protocol`, feeds the terminal bytes into the engine via
`phux-vt-web`, paints the grid (with a blinking cursor), and sends keystrokes
back as `INPUT_KEY` frames. Both transports carry the identical wire; the
WebTransport stream is length-prefixed frames reassembled by
`framing::FrameBuffer`, a WebSocket message is one frame.

After `HELLO_OK` the client picks a session before it attaches. A server
advertising `ORGANIZATION` is asked for its organization page and the client
opens the owner's Intake session; any other server is asked for its state and
the client opens its most recent session. Only a server with no sessions at all
gets a `default` session created.

The `start_webtransport` entry point supervises post-connect transport loss:
each replacement attempt has bounded WT and HELLO setup, then retries through
the authenticated WebSocket fallback after a short delay. Direct `run*` Rust
callers receive a failure-visible `Client` and may reconnect explicitly.

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

## Public API

`#[wasm_bindgen]` entry points, designed to be driven from JS:

```js
import init, { start, start_hosted, start_webtransport } from "./pkg/phux_web.js";
await init();
// finds <canvas id="…">, connects, attaches, and runs for the connection's life
await start("wss://host/session", "my-canvas", /*cols*/ 100, /*rows*/ 24);
// or WebTransport-first (phux server --webtransport), WebSocket fallback;
// on a token-authenticated listener append ?token=<hex> to the https URL.
// Fallback carries that token in Sec-WebSocket-Protocol, never the WSS URL:
await start_webtransport("https://host:4433/session", "wss://host/session",
                         "my-canvas", 100, 24);
// hosted live-demo entry returns a controller
const client = await start_hosted(url, "my-canvas", 100, 24, onEvent, signal);
client.split_pane("vertical"); // side-by-side; "horizontal" stacks panes
client.focus_next_pane();
client.close_pane();           // closes the view only; the terminal keeps running
client.kill_pane();            // ends the focused terminal; keeps at least one pane
client.send_key("Escape");     // an on-screen key: named key or one character
client.set_ctrl_latch(true);   // Ctrl for the next typed key; fires phux-modifiers
// client.resize(cols, rows) resizes the view; client.close() releases it
```

Input, touch, scrollback, selection, find, mouse and focus reporting, links,
the bell and title events, and the connection attribute are described in
[the web client guide](../../docs/consumers/web.md#in-the-page).

## Building

Follow [Contributor setup](../../docs/SETUP.md#browser-client) for native or Nix
tools. The engine module is committed, so ordinary client work needs only the
Rust/WASM tools:

```sh
# build this client to a web package using the committed engine:
cd clients/phux-web && wasm-pack build --target web --release --out-dir pkg
#    → pkg/phux_web.js + pkg/phux_web_bg.wasm  (~2 MB, ~600 KB gzipped;
#      the embedded engine is 1.5 MB of it)
```

Tool versions and installation commands live in the setup guide;
`bash scripts/doctor.sh web` checks them from the repository root.
Changing the engine itself additionally needs Zig; the setup guide documents
verified source acquisition and the byte-for-byte regeneration check.

## Tests

```sh
wasm-pack test --node                 # session/codec, input, search, and link routing
# headless Chrome suites against a fresh ws_demo_server, as CI runs them
# (from the repository root):
nix develop .#browser -c python3 scripts/ci/web-browser.py
```

## Scope

Up to four independent terminal panes on one transport, with text, color, and
cursor rendering. Pane controls, shortcuts, and events are documented in
[the web client guide](../../docs/consumers/web.md#in-the-page). Images
(sixel/Kitty graphics, which the engine parses) are not drawn.

[ADR-0017]: ../../docs/adr/0017-tui-not-protocol-privileged.md
[ADR-0024]: ../../docs/adr/0024-wire-owns-input-atoms.md
[ADR-0025]: ../../docs/adr/0025-browser-web-client.md

## License

Apache-2.0.

## Hosted pane presentation

`HostedClient` owns one connection and its validated terminal replicas. Keep
that client and its canvas alive when promoting a preview into a workspace;
calling `start_hosted` again opens another connection.

The canvas emits bubbling `CustomEvent`s with generated TypeScript types:

- `phux-panes`: `PaneState`, retaining `count`, `focused`, `pending` and optional
  `error`, with `panes: PaneDescriptor[]` and total cell `cols`/`rows`.
- `phux-render`: `PaneRenderDetail`, emitted after a full real canvas paint.
  It carries the same canonical pane rectangles plus backing-store
  `pixelWidth`/`pixelHeight`. Each rectangle has `id`, `x`, `y`, `cols`, `rows`
  in cells and excludes separators. Optional `cursorRow` is the pane-local
  engine cursor row from the last full paint, reusing grids already read by the
  renderer. Clamp it to the visible rectangle during a pending resize.
  `phux-panes` may omit it before the first paint. The event is dispatched without an App
  borrow, so a listener can call the client. Cursor-only blink redraws do not
  emit this presentation event; no perpetual preview-copy timer is needed.

A preview can crop the painted source canvas using these actual rectangles.
This shares the existing terminal replicas; it does not create shells, expose
raw transport bytes, or add another subscription.

`focus_pane(id)` accepts an exact canonical id from a visible descriptor,
including satellite selectors, and rejects hidden or unready terminals.
`send_text(text)` sends one structured, trusted paste to the focused terminal:
at most 4096 UTF-8 bytes, empty text is a no-op, and no Enter key is appended.
Use `send_key("Enter")` separately when intended. Backend command support and
session admission remain the server's responsibility.

Focused browser regressions are named `presentation_*`. Four run without a
server fixture. The ignored structured-paste wire test additionally needs an
owned loopback WebSocket echo fixture set at build time with
`PHUX_PRESENTATION_ECHO_URL`, then `--include-ignored`; it decodes the actual
sent PHUX frame and does not use a remote shell.
