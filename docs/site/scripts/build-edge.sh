#!/usr/bin/env bash
# build-edge.sh — compile the phux-edge WASM server and stage it for the Worker.
#
# phux-edge (edge/) is the phux server compiled to WASM: it speaks the real phux
# wire (phux-protocol) and is backed by a curated shell. The SessionDO imports
# the output. We commit the built artifact (worker/edge/) so `wrangler deploy`
# and CI don't need the Rust/wasm toolchain.
#
# Requires the phux nix devshell (rust 1.98 + wasm-pack), or a local
# rustup + wasm-pack. Re-run when edge/ changes.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo/edge"

echo "building phux-edge → wasm ..."
wasm-pack build --target web --release --out-dir pkg

out="$repo/worker/edge"
mkdir -p "$out"
cp pkg/phux_edge.js pkg/phux_edge_bg.wasm pkg/phux_edge.d.ts "$out/"
echo "staged → worker/edge/ ($(du -h "$out/phux_edge_bg.wasm" | cut -f1) wasm)"
