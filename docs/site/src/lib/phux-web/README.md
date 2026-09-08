# phux-web (generated)

Do not edit by hand. This is the **real** phux browser client — the phux repo's
`clients/phux-web` compiled to wasm (`wasm-pack --target web`), which embeds the
real `libghostty-vt` engine (`ghostty-vt.wasm`). The `<PhuxTerminal>` React
island (`src/components/PhuxTerminal.tsx`, via `terminal/core.ts`) dynamically
imports it on mount.

The committed artifacts are built from hosted-client backport revision
`fe1f9563b44f0abf46a75b88d7e4382681f660d4`, based on deployed protocol-0.5
revision `1f2501f979be972886a1adb805bb598b9189e2f9`. The native server remains
pinned to that protocol revision in `worker/Dockerfile`; the build script rejects
current protocol-0.8 sources.

It is committed (not built in CI) because Cloudflare Pages' build image has no
Rust/Zig toolchain. Regenerate it from a checkout of the phux repo next door:

```sh
bun run build:client     # → scripts/build-client.sh
```

`phux_web_bg.wasm` is ~6 MB (client + engine). It is a dynamic import, so it
ships only when a visitor clicks Launch — idle pageviews never download it.
