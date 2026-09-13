#!/usr/bin/env bash
# build-vt-wasm.sh — build the checkpoint-capable standalone libghostty-vt WASM
# module and vendor it into the browser engine adapter.
#
# The browser instantiates the immutable protocol-0.7 checkpoint-v2 engine as a
# second WASM module. It imports env.log plus ghostty.host_entropy_fill; the
# Rust adapter supplies secure browser entropy and probes codec identity,
# version, features, and limits before advertising NativeState.
#
# Requires the release-pinned Zig and Node. Byte-for-byte reproduction assumes
# the official Zig release binary (scripts/install-zig.sh): nixpkgs' zig_0_16 on
# x86_64 Linux links a different LLVM build and compiles one function
# differently, so the Nix shell's Zig fails --check there. The recipe pins
# --seed 0, -j1, and isolated caches so a random dependency-walk seed or a
# shared Zig cache cannot change the artifact. By default fetches verified
# immutable source; GHOSTTY_SRC is an explicit local development override.
# --check rebuilds and compares without changing the committed artifact.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
source "$repo/scripts/lib/dev-toolchain.sh"
revision=392baed9cbf0f572d551c4b3e0c4f5c40bcca054
archive_sha256=64198f7469a0d3d79455c0f8434ae1fbb50482dac69f82f1d8e08f4d6b5e6ea5
mode="${1:-build}"
[[ $# -le 1 && ( "$mode" = build || "$mode" = --check ) ]] || {
  echo 'usage: bash scripts/build-vt-wasm.sh [--check]' >&2; exit 2;
}
for tool in zig node tar; do
  command -v "$tool" >/dev/null || { echo "$tool missing; see docs/SETUP.md#browser-client" >&2; exit 1; }
done
[[ "$(zig version)" = "$ZIG_VERSION" ]] || { echo "requires Zig $ZIG_VERSION" >&2; exit 1; }
digest() {
  if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1
}
scratch="$(mktemp -d "${TMPDIR:-/tmp}/phux-vt-wasm.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
if [[ -z "${GHOSTTY_SRC:-}" ]]; then
  curl -fL --retry 3 "https://codeload.github.com/phall1/ghostty/tar.gz/$revision" -o "$scratch/source.tar.gz"
  [[ "$(digest "$scratch/source.tar.gz")" = "$archive_sha256" ]] || { echo 'engine source checksum mismatch' >&2; exit 1; }
  tar -xf "$scratch/source.tar.gz" -C "$scratch"
  GHOSTTY_SRC="$scratch/ghostty-$revision"
fi
[[ -f "$GHOSTTY_SRC/build.zig" ]] || { echo "ghostty source missing: $GHOSTTY_SRC" >&2; exit 1; }
GHOSTTY_SRC="$(cd "$GHOSTTY_SRC" && pwd)"

echo "building ghostty-vt.wasm from $GHOSTTY_SRC (zig $(zig version)) ..."
# Fix version metadata and keep runtime safety checks in the shipping engine.
# This revision ignores -Dstrip for VT WASM, so prepare-vt-wasm removes custom
# metadata sections explicitly after compilation.
( cd "$GHOSTTY_SRC" && zig build -Demit-lib-vt -Dtarget=wasm32-freestanding \
    -Doptimize=ReleaseSafe -Dstrip=true -Dversion-string=1.3.2-dev \
    --seed 0 -j1 \
    --cache-dir "$scratch/zig-cache" --global-cache-dir "$scratch/zig-global" \
    --prefix "$scratch/out" )
artifact="$scratch/out/bin/ghostty-vt.wasm"
node "$repo/scripts/prepare-vt-wasm.mjs" "$artifact"
( cd "$GHOSTTY_SRC" && node test/lib_vt_snapshot_incremental_wasm.mjs "$artifact" )

dest="$repo/clients/phux-vt-web/vendor/ghostty-vt.wasm"
if [[ "$mode" = --check ]]; then
  cmp "$artifact" "$dest" || {
    echo "rebuilt sha256 $(digest "$artifact"); committed sha256 $(digest "$dest"); zig $(command -v zig)" >&2
    echo 'engine differs; regenerate with bash scripts/build-vt-wasm.sh' >&2; exit 1;
  }
  echo 'committed engine matches the verified source rebuild'
  exit 0
fi
mkdir -p "$(dirname "$dest")"
cp "$artifact" "$dest"
echo "vendored $(du -h "$dest" | cut -f1) -> ${dest#"$repo"/}"
