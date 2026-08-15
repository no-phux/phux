---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-08-15
---

# 0025 — Browser web client over a WebSocket transport

**TL;DR.** `phux-web` is a browser consumer of the wire (ADR-0017: consumers are
not protocol-privileged), built in Rust→WASM. It speaks the **exact**
`phux-protocol` codec (wasm-safe per ADR-0024) and renders with the **exact**
`libghostty-vt` engine — `ghostty-vt.wasm` loaded as a self-contained module and
driven from Rust (`phux-vt-web`), *not* zig linked into the wasm binary, and
*not* a JS terminal re-implementation. To reach it, the server grows a
frame-level `Transport` abstraction with a **WebSocket** impl alongside UDS
(one binary message = one `FrameKind`); the per-client dispatch loop and codec
are transport-agnostic. The client is single-terminal: `HELLO` → `ATTACH` →
feed `TERMINAL_SNAPSHOT`/`TERMINAL_OUTPUT` into the engine → paint the grid to a
`<canvas>`; keystrokes become `INPUT_KEY`. Rendering is **not** shared with the
multi-pane ratatui TUI, but the session kernel is: `phux-web` depends on
`phux-client-core`, reversing this ADR's original no-shared-core call.

Status: Accepted
Date: 2026-05-30

## Context

ADR-0017 frames the TUI as one consumer among peers (agents, an SDK, a browser),
all speaking the same wire. Two things blocked a browser peer: the wire codec
was libghostty-coupled and unbuildable on wasm (fixed by ADR-0024), and the
server only listened on a Unix domain socket. Separately, a browser terminal
needs a VT engine; xterm.js-class renderers drop exactly the modern protocols
(kitty graphics, sixel, kitty keyboard) that are phux's whole point, while
ghostty already compiles its VT engine to a standalone `ghostty-vt.wasm` module.

## Decision

1. **Server `Transport` seam.** `phux-server` abstracts its accept loop behind a
   frame-level `FrameReader`/`FrameWriter`/`Incoming` trait set. UDS frames stay
   length-prefixed on the byte stream; a WebSocket transport carries one encoded
   `FrameKind` per binary message. The dispatch loop and codec are unchanged.
   WebSocket is opt-in via `PHUX_WS_ADDR`; UDS is always on.
2. **Engine reuse, not reimplementation.** `phux-vt-web` loads `ghostty-vt.wasm`
   (self-contained: its only import is `env.log`, it ships its own allocator) via
   the WebAssembly JS API and exposes a safe Rust surface over the
   `libghostty-vt` C ABI. The browser renders with the same engine native phux
   uses.
3. **Codec reuse.** `phux-web` depends on `phux-protocol` (default, wasm-safe)
   and speaks the real `FrameKind` wire — no parallel JS/TS protocol.
4. **Single-terminal consumer.** The client attaches to a named `default`
   session, mirrors one terminal, and paints its grid to a `<canvas>`. Splits,
   layout, and keybind chrome (the multi-pane TUI's job) are out of scope; the
   browser is a thin, focused projection.

## Rationale

Reusing the codec and the engine is what makes the browser a *peer* rather than
a lookalike: it gets every terminal protocol for free, forever, exactly as the
native client does. The frame-level transport seam keeps the wire identical
across UDS and WebSocket, so the server has one dispatch path, not two. Loading
ghostty-vt as a separate module (rather than linking zig into the Rust wasm
binary) sidesteps the rust+zig single-linear-memory problem and tracks how
ghostty intends its wasm engine to be embedded.

## Tradeoffs

- The browser↔engine boundary copies bytes across two wasm linear memories (the
  Rust client and `ghostty-vt.wasm`). Fine for a terminal; the @wterm ecosystem
  proves the model.
- The renderers stay separate — canvas vs VT-to-stdout plus ratatui chrome — so
  the paint layer is written twice. The decode-and-feed step is not: both
  consumers drive `phux-client-core`'s session kernel, so replica lifecycle,
  generation validation, ordering, and READY fences exist once. See the
  "shared `phux-client-core`" entry under Alternatives for how that changed.

## Alternatives considered

- **A shared `phux-client-core` for native + web.** The original plan, descoped
  here on the reasoning that the shared surface was tiny. That sizing was wrong
  and this decision has since been reversed in tree. The crate was extracted for
  the native client on 2026-05-31 — to fence `ratatui` behind a
  compiler-enforced boundary ([ADR-0020](0020-layered-render.md)), not for the
  browser — and `phux-web` adopted it on 2026-08-04. It is roughly 16.7k lines
  across six modules, of which `phux-web` imports three: `engine`, `history`,
  and `session`. What survives from this ADR is the narrower rendering claim:
  the two consumers' renderers do differ entirely, and they are still not
  shared. What was falsified is the conclusion drawn from it.
- **A JS terminal (xterm.js) fed by the wire.** Rejected: it drops the modern
  protocols that distinguish phux, and reimplements the wire in TypeScript.

[ADR-0017]: 0017-tui-not-protocol-privileged.md
[ADR-0024]: 0024-wire-owns-input-atoms.md
