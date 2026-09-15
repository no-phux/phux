# phux-web (generated)

Do not edit by hand. This is the **real** phux browser client — the phux repo's
`clients/phux-web` compiled to wasm (`wasm-pack --target web`), which embeds the
real `libghostty-vt` engine (`ghostty-vt.wasm`). The `<PhuxTerminal>` React
island (`src/components/PhuxTerminal.tsx`, via `terminal/core.ts`) dynamically
imports it on mount.

The committed artifacts are built from in-repo `clients/phux-web` at workspace
protocol 0.9, matching the native image pin and `phux-edge`. They include
`start_hosted` for the Worker session envelope.

It is committed (not built in CI) because Cloudflare Pages' build image has no
Rust/Zig toolchain. Regenerate it from this checkout:

```sh
bun run build:client     # → scripts/build-client.sh
```

`phux_web_bg.wasm` is ~2 MB (client; the VT engine is the committed
`ghostty-vt.wasm`). It is a dynamic import, so it ships only when a visitor
clicks Launch — idle pageviews never download it.
