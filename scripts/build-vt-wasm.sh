#!/usr/bin/env bash
# build-vt-wasm.sh — build the checkpoint-capable standalone libghostty-vt WASM
# module and vendor it into the browser engine adapter.
#
# The browser instantiates the immutable protocol-0.7 checkpoint-v2 engine as a
# second WASM module. It imports env.log plus ghostty.host_entropy_fill; the
# Rust adapter supplies secure browser entropy and probes codec identity,
# version, features, and limits before advertising NativeState.
#
# Requires Node, plus the official Zig release binary, which this script
# installs itself with scripts/install-zig.sh (digest-verified against
# .config/zig-toolchain.json) under PHUX_TOOLCHAIN_DIR and runs by absolute
# path: no Zig on PATH is ever used. nixpkgs' zig_0_16 on x86_64 Linux links a
# different LLVM build and deterministically compiles one function differently.
# The recipe pins --seed 0, -j1, and isolated caches so a random
# dependency-walk seed or a shared Zig cache cannot change the artifact.
#
# Even the official Zig 0.16.0 is not fully deterministic: with the same
# binary, source, and flags, roughly one cold build in twenty on hosted runners
# emits different code (-j1 limits build steps, not the compiler's own
# threads). So --check rebuilds from fresh caches up to three times and passes
# when any build reproduces the committed bytes; regenerating needs two builds
# that agree.
#
# By default fetches verified immutable source; GHOSTTY_SRC is an explicit
# local development override. --check rebuilds and compares without changing
# the committed artifact.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
source "$repo/scripts/lib/dev-toolchain.sh"
revision=392baed9cbf0f572d551c4b3e0c4f5c40bcca054
archive_sha256=64198f7469a0d3d79455c0f8434ae1fbb50482dac69f82f1d8e08f4d6b5e6ea5
mode="${1:-build}"
[[ $# -le 1 && ( "$mode" = build || "$mode" = --check ) ]] || {
  echo 'usage: bash scripts/build-vt-wasm.sh [--check]' >&2; exit 2;
}
for tool in node tar; do
  command -v "$tool" >/dev/null || { echo "$tool missing; see docs/SETUP.md#browser-client" >&2; exit 1; }
done
toolchains="${PHUX_TOOLCHAIN_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/phux/toolchains}"
zig="$("$BASH" "$repo/scripts/install-zig.sh" "$toolchains")/zig"
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

# Build, normalize, and ABI-test one engine from fresh caches into
# $scratch/out-N/bin/ghostty-vt.wasm. Only fetched packages carry over.
rebuild() {
  local out="$scratch/out-$1" global="$scratch/zig-global-$1"
  mkdir -p "$global"
  [[ ! -d "$scratch/zig-global-1/p" ]] || cp -R "$scratch/zig-global-1/p" "$global/"
  echo "building ghostty-vt.wasm (attempt $1) from $GHOSTTY_SRC ($zig $("$zig" version)) ..."
  # Fix version metadata and keep runtime safety checks in the shipping engine.
  # This revision ignores -Dstrip for VT WASM, so prepare-vt-wasm removes custom
  # metadata sections explicitly after compilation.
  ( cd "$GHOSTTY_SRC" && "$zig" build -Demit-lib-vt -Dtarget=wasm32-freestanding \
      -Doptimize=ReleaseSafe -Dstrip=true -Dversion-string=1.3.2-dev \
      --seed 0 -j1 \
      --cache-dir "$scratch/zig-cache-$1" --global-cache-dir "$global" \
      --prefix "$out" )
  node "$repo/scripts/prepare-vt-wasm.mjs" "$out/bin/ghostty-vt.wasm"
  ( cd "$GHOSTTY_SRC" && node test/lib_vt_snapshot_incremental_wasm.mjs "$out/bin/ghostty-vt.wasm" )
}

dest="$repo/clients/phux-vt-web/vendor/ghostty-vt.wasm"
if [[ "$mode" = --check ]]; then
  for attempt in 1 2 3; do
    rebuild "$attempt"
    artifact="$scratch/out-$attempt/bin/ghostty-vt.wasm"
    if cmp -s "$artifact" "$dest"; then
      echo "committed engine matches the verified source rebuild (attempt $attempt)"
      exit 0
    fi
    cmp "$artifact" "$dest" >&2 || true
    echo "attempt $attempt: rebuilt sha256 $(digest "$artifact"); committed sha256 $(digest "$dest")" >&2
  done
  echo "engine differs in 3 rebuilds with $zig; if they agree with each other, regenerate with bash scripts/build-vt-wasm.sh" >&2
  exit 1
fi
rebuild 1
rebuild 2
artifact="$scratch/out-1/bin/ghostty-vt.wasm"
cmp -s "$artifact" "$scratch/out-2/bin/ghostty-vt.wasm" || {
  echo "two rebuilds differ ($(digest "$artifact") vs $(digest "$scratch/out-2/bin/ghostty-vt.wasm")); run again" >&2
  exit 1
}
mkdir -p "$(dirname "$dest")"
cp "$artifact" "$dest"
echo "vendored $(du -h "$dest" | cut -f1) -> ${dest#"$repo"/}"
