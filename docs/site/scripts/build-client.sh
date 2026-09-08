#!/usr/bin/env bash
# Regenerate the live-demo wasm client (src/lib/phux-web/) from the phux repo.
#
# The demo island imports the REAL phux-web browser client — clients/phux-web
# compiled to wasm, which embeds the real libghostty-vt engine. That artifact
# can't be built in Cloudflare Pages' build image (no Rust/Zig/nix), so we build
# it here and commit the output. Re-run this whenever the phux client changes.
#
# Requires the phux repo checked out next to this one and its nix devshell
# (provides rust, zig, wasm-pack, wasm-bindgen-cli, binaryen).
set -euo pipefail

SITE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PHUX_DIR="${PHUX_DIR:-$(cd "$SITE_DIR/../phux" && pwd)}"
OUT="$SITE_DIR/src/lib/phux-web"
EXPECTED_REVISION="fe1f9563b44f0abf46a75b88d7e4382681f660d4"

echo "phux repo:  $PHUX_DIR"
echo "site out:   $OUT"

if [ ! -d "$PHUX_DIR/clients/phux-web" ]; then
  echo "error: clients/phux-web not found in $PHUX_DIR (set PHUX_DIR)" >&2
  exit 1
fi
actual_revision="$(git -C "$PHUX_DIR" rev-parse HEAD)"
if [ "$actual_revision" != "$EXPECTED_REVISION" ]; then
  echo "error: phux-web must be built from hosted protocol-0.5 revision $EXPECTED_REVISION" >&2
  echo "found: $actual_revision" >&2
  exit 1
fi

nix develop "$PHUX_DIR" --command bash -c '
  set -euo pipefail
  cd "'"$PHUX_DIR"'"
  # The engine module is a gitignored build artifact; build it if missing.
  if [ ! -f clients/phux-vt-web/vendor/ghostty-vt.wasm ]; then
    echo "building ghostty-vt.wasm (engine)…"
    bash scripts/build-vt-wasm.sh
  fi
  cd clients/phux-web
  wasm-pack build --target web --release --out-dir pkg
'

mkdir -p "$OUT"
cp "$PHUX_DIR"/clients/phux-web/pkg/phux_web.js \
   "$PHUX_DIR"/clients/phux-web/pkg/phux_web_bg.wasm \
   "$PHUX_DIR"/clients/phux-web/pkg/phux_web.d.ts \
   "$PHUX_DIR"/clients/phux-web/pkg/phux_web_bg.wasm.d.ts \
   "$OUT/"

echo "done. wrote:"
ls -lh "$OUT"
