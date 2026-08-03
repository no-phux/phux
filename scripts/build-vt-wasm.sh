#!/usr/bin/env bash
# build-vt-wasm.sh — build the checkpoint-capable standalone libghostty-vt WASM
# module and vendor it into the browser engine adapter.
#
# The browser instantiates the immutable protocol-0.7 checkpoint-v2 engine as a
# second WASM module. It imports env.log plus ghostty.host_entropy_fill; the
# Rust adapter supplies secure browser entropy and probes codec identity,
# version, features, and limits before advertising NativeState.
#
# Requires zig 0.16.x (the phux nix devshell provides it). GHOSTTY_SRC defaults
# to the published standalone checkpoint-WASM checkout; override it only with
# a source tree implementing the same frozen incremental ABI.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
GHOSTTY_SRC="${GHOSTTY_SRC:-$repo/../ghostty-checkpoint-wasm}"

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
