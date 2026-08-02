#!/usr/bin/env bash
# build-vt-wasm.sh — build ghostty's libghostty-vt as a standalone wasm module
# and vendor it into the phux-web client (phux-486.2).
#
# The phux browser client renders terminals with this exact engine. The module
# is self-contained (only import: env.log) and ships its own allocator; the
# Rust driver in clients/phux-vt-web loads + drives it via the WebAssembly API.
#
# Requires zig 0.16.x (the phux nix devshell provides it). Point GHOSTTY_SRC at
# a ghostty checkout (default ../ghostty). Ideally pin it to the same rev
# libghostty-vt-sys uses; see crates Cargo.toml.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
GHOSTTY_SRC="${GHOSTTY_SRC:-$repo/../ghostty}"

if [ ! -f "$GHOSTTY_SRC/build.zig" ]; then
  echo "ghostty source not found at GHOSTTY_SRC=$GHOSTTY_SRC" >&2
  echo "  set GHOSTTY_SRC=/path/to/ghostty" >&2
  exit 1
fi
if ! command -v zig >/dev/null 2>&1; then
  echo "zig not on PATH — run inside the nix devshell (nix develop)" >&2
  exit 1
fi

echo "building ghostty-vt.wasm from $GHOSTTY_SRC (zig $(zig version)) ..."
( cd "$GHOSTTY_SRC" && zig build -Demit-lib-vt -Dtarget=wasm32-freestanding )

dest="$repo/clients/phux-vt-web/vendor/ghostty-vt.wasm"
mkdir -p "$(dirname "$dest")"
cp "$GHOSTTY_SRC/zig-out/bin/ghostty-vt.wasm" "$dest"
echo "vendored $(du -h "$dest" | cut -f1) -> ${dest#"$repo"/}"
