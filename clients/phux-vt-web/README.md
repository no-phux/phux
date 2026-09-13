# phux-vt-web

A thin, safe **Rust driver for ghostty's `libghostty-vt` terminal engine,
compiled to WebAssembly** (`ghostty-vt.wasm`).

It does one thing: load the engine module and give Rust a small, safe surface
over its C ABI — make a terminal, write VT bytes into it, read the styled grid
(cells + fg/bg + cursor) back out. No networking, no protocol, no DOM. It is the
engine half of the [phux](https://github.com/no-phux/phux) browser client; the
client half is [`phux-web`](../phux-web).

```text
ghostty-vt.wasm  (the real VT engine, from ghostty/Zig)
      ▲  WebAssembly JS API (instantiate + call C ABI)
      │
  phux-vt-web  (this crate — a safe Rust wrapper)
      │  Vt / Terminal / Grid
      ▼
  your renderer
```

## Why

A terminal "screen" is produced by a **VT engine** — the thing that turns raw
bytes like `\033[1;32mhi` into "green bold `hi`." Rather than reimplement one in
JS (xterm.js-style), phux runs **ghostty's actual engine** in the browser, so
every feature — truecolor, OSC 8 hyperlinks, Kitty keyboard, grapheme
clustering, sixel parsing — behaves exactly as it does natively, for free.

The engine ships as a **self-contained** wasm module: it imports `env.log` and
`ghostty.host_entropy_fill`, and bundles its own allocator. This crate loads it through the
plain `WebAssembly` JS API and drives its C ABI — **no Zig is linked into the
Rust binary** (that would mean sharing one wasm linear memory between two
toolchains; instead we run the engine as a sibling module and copy bytes across
the boundary). See phux ADR-0025.

## API

```rust
use phux_vt_web::Vt;

let vt = Vt::load().await?;          // instantiate ghostty-vt.wasm
let mut term = vt.terminal(80, 24);  // a terminal of cols × rows
term.write(b"\x1b[1;32mhello\x1b[0m\n");
let grid = term.grid();              // styled snapshot, ready to render
// grid.cells[r*cols + c] → { ch, fg, bg };  grid.cursor_col / cursor_row / cursor_visible
```

`Vt` owns the engine instance; `Terminal` is one screen on it; `Grid` is a
row-major snapshot a renderer paints. That's the whole surface.

## The engine artifact (build dependency)

This crate `include_bytes!`s the committed `vendor/ghostty-vt.wasm` at compile
time. Ordinary client builds reuse it without Zig. To regenerate the engine
from verified pinned source:

```sh
# from the phux repo root, with the release-pinned Zig (scripts/install-zig.sh) on PATH:
bash scripts/build-vt-wasm.sh          # → clients/phux-vt-web/vendor/ghostty-vt.wasm
bash scripts/build-vt-wasm.sh --check  # byte-for-byte regeneration check
```

`build.rs` errors with a clear message if the artifact is missing. Follow
[Contributor setup](../../docs/SETUP.md#browser-client) for tool versions,
source pin maintenance, and local engine development overrides.
`bash scripts/doctor.sh web` checks tools and artifact presence from the root;
the tests verify engine ABI compatibility.

## Tests

```sh
wasm-pack test --node     # drives the real engine; reads a truecolor cell + cursor back
```

## License

Apache-2.0.
