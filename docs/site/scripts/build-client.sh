#!/usr/bin/env bash
# Regenerate the live-demo wasm client (src/lib/phux-web/) from this phux checkout.
#
# The demo island imports the REAL phux-web browser client — clients/phux-web
# compiled to wasm, which embeds the real libghostty-vt engine. That artifact
# can't be built in Cloudflare Pages' build image (no Rust/Zig/nix), so we build
# it here and commit the output. Re-run this whenever the phux client changes.
#
# Requires rustc + wasm32, wasm-pack, wasm-bindgen-cli 0.2.128, and the committed
# ghostty-vt.wasm engine (see docs/SETUP.md § Browser client).
set -euo pipefail

SITE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHUX_DIR="${PHUX_DIR:-$(cd "$SITE_DIR/../.." && pwd)}"
OUT="$SITE_DIR/src/lib/phux-web"

echo "phux repo:  $PHUX_DIR"
echo "site out:   $OUT"

if [ ! -d "$PHUX_DIR/clients/phux-web" ]; then
  echo "error: clients/phux-web not found in $PHUX_DIR (set PHUX_DIR)" >&2
  exit 1
fi
if [ ! -f "$PHUX_DIR/clients/phux-vt-web/vendor/ghostty-vt.wasm" ]; then
  echo "error: committed engine missing at clients/phux-vt-web/vendor/ghostty-vt.wasm" >&2
  echo "restore it from git or rebuild with bash scripts/build-vt-wasm.sh" >&2
  exit 1
fi

(
  cd "$PHUX_DIR/clients/phux-web"
  wasm-pack build --target web --release --out-dir pkg
)

mkdir -p "$OUT"
cp "$PHUX_DIR"/clients/phux-web/pkg/phux_web.js \
   "$PHUX_DIR"/clients/phux-web/pkg/phux_web_bg.wasm \
   "$PHUX_DIR"/clients/phux-web/pkg/phux_web.d.ts \
   "$PHUX_DIR"/clients/phux-web/pkg/phux_web_bg.wasm.d.ts \
   "$OUT/"

echo "done. wrote:"
ls -lh "$OUT"
