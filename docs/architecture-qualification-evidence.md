---
group: internals
order: 9.1
---

# Architecture qualification evidence

What actually ran against the integrated `refactor/architecture-end-state`
source, measured against the [qualification](./architecture-qualification.md)
contract. A pass recorded here covers the source named below, not a later
tree. Anything listed under **not executed** is still required; nothing here
is borrowed from an older run.

## Source and environment

- Source: `refactor/architecture-end-state`, merged with `origin/main` at
  `eddc063be`. Phase checkpoints `2622d0573` (compile/clippy frontier) and
  `299806e6c` (qualification fixes); Rune re-qualified after the final merge.
- Host: Apple M4 Pro, macOS 27.0.1, rustc 1.99.1, Xcode SDK MacOSX27.0,
  Android NDK 28.2.13676358. Isolated `CARGO_TARGET_DIR` per lane; production
  `PHUX_*` variables stripped; every server on a disposable socket.

## Executed

| Lane | Command shape | Result |
| --- | --- | --- |
| Format | `cargo fmt --all --check` (root, Rune, browser) | clean |
| Lint | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | clean |
| Rustdoc | `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features` | clean |
| Build shapes | lean `phux --no-default-features`; `phux-client`+`phux-mcp` no-default with testkit; `phux-tui` no-default; `phux-client-runtime` no-default; UniFFI shape; `bun tools/main.ts build-features-check`; `cargo deny check` | all pass |
| Wire/docs | `PHUX_UPDATE_WIRE_SCHEMA=1 … wire_schema_matches`; `phux gen-reference-docs`; `scripts/check-docs.sh` + fixtures | schema and references regenerated, docs gate passes |
| Root suite | `cargo nextest run --workspace --features phux-server/measure` | 7155 run; 7154 passed. `spawn_terminal::viewport_resize_reaches_nonactive_panes_and_preserves_noop_generations` failed once under full-suite load, then passed 40/40 alone and 150/150 under a concurrent release build; that test is unchanged from `main` |
| e2e | `just e2e` selection (ignored PTY/server/automation cases), serial | 193/193 |
| Stress | `just stress` | 10/10 |
| Protocol | `cargo nextest run -p phux-protocol` | 342/342 |
| Binding | `cargo test -p phux-client-ffi --no-default-features --features uniffi --lib` | 108/108 |
| Rune | `clients/rune/scripts/check.sh` (fmt, clippy, tests, Python, Bun) | 1174 Rust (1 ignored), 288 Python, 5 Bun |
| Browser | `wasm-pack test --node` for `phux-vt-web` and `phux-web`; shipping `wasm-pack build --release`; VT ABI node tests | all pass; package builds |
| Apple FFI | `scripts/build-mobile-ffi-xcframework.sh --full` | XCFramework, generated Swift, design and provenance written; Swift smoke `phux-client-ffi-smoke: ok` |
| Android FFI | `scripts/build-mobile-ffi-android.sh` | Kotlin, `arm64-v8a`/`x86_64` libraries and provenance written |
| Rune iOS | `clients/rune/ios/build.py --target simulator`, then `qualify.py` over QUIC and over WSS against a same-source debug `phux` | both passed on a disposable simulator: input ready, injected key, background and resume reconnect |
| CLI and MCP | real `phux` and `phux mcp` against a disposable server | ensure, create, name collision refused, `ls`, `run`, `send-keys`, `snapshot`, unconfirmed kill refused, session kill reports `accepted: true, end_observed: false`; MCP lists 77 tools and answers `phux_ls`/`phux_snapshot` |
| Native echo | `scripts/bench/tui-load.sh`, release, 188×48, 60 keys, two alternating rounds against `main` | quiet p50 516–528 µs vs 560–564 µs; full-flood p50 586–591 µs vs 571–579 µs; p90 equal; server RSS and CPU equal |
| Rune echo | `scripts/bench/rune-echo.sh`, release, 200 keys, headless at 1000 fps (five alternating rounds) and 60 fps (two) | key to drawn frame p50 median 2190 µs vs 2177 µs on `main`, p99 4.3–4.5 ms on both; server hops identical |

## Behaviour this run established

- Archive import is inert. `phux restore` and `phux relaunch` refuse a recipe
  no current trusted template admits, start nothing, and name the exact
  `phux relaunch ^KEY --approve-raw` review; approval then runs the recipe.
  A recipe the destination's current template admits restores directly.
- A launch whose own effect failed while the server stopped is recorded as
  interrupted and restorable; a Starting record without that evidence
  reconciles as Lost, never as never-launched.
- Restarted clients recover a session only by its durable authority and key.
  Reusing an operation key for a different request is refused.

## Not executed

- Chrome lane (`bun tools/main.ts web-browser`): local Chrome 158 does not
  match ChromeDriver 156. CI runs it.
- Rune iOS on a physical device, photon latency (needs Screen Recording and Accessibility grants on an
  unlocked display) and Android interaction on a device or emulator.
- Foreign-language scenarios beyond the Swift smoke and the Rust-side UniFFI
  tests; released-peer compatibility matrix; 24-hour soak; the twelve
  change-locality rehearsals.
- Raw approval is bounded at 16 KiB and binds the server's inherited
  environment, so a server started from a large shell environment refuses
  approval (it fails closed). Tracked separately.
