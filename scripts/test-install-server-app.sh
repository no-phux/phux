#!/usr/bin/env bash
# scripts/install.sh on macOS also installs phux Server.app beside the CLI:
# Versions/v<version> plus Versions/Current under ~/Library/Application
# Support/sh.phux/Server, pruning all but the previous release. The network
# is a fake curl serving $SERVED by basename; HOME is a temp directory.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
SH="$(command -v dash || echo /bin/sh)"
TARGET=aarch64-apple-darwin
export HOME="$TMP/home" SERVED="$TMP/served" MISE_TRUSTED_CONFIG_PATHS="$ROOT"
unset PHUX_CHANNEL PHUX_INSTALL_DIR PHUX_REQUIRE_SIGNATURE
mkdir -p "$HOME" "$TMP/bin" "$SERVED"
cat > "$TMP/bin/curl" <<'CURL'
#!/bin/sh
out="" url=""
while [ "$#" -gt 0 ]; do
  case "$1" in -o | -O) out="$2"; shift ;; https://*) url="$1" ;; esac
  shift
done
[ -f "$SERVED/${url##*/}" ] || exit 22
if [ -n "$out" ] && [ "$out" != - ]; then cp "$SERVED/${url##*/}" "$out"; else cat "$SERVED/${url##*/}"; fi
CURL
chmod 755 "$TMP/bin/curl"

fail() { echo "FAIL: $*" >&2; exit 1; }
install_mac() {
  PATH="$TMP/bin:/usr/bin:/bin" "$SH" "$ROOT/scripts/install.sh" --os darwin --arch arm64 \
    --install-dir "$TMP/cli" --no-modify-path "$@"
}
sha() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | awk '{ print $1 }'; }

# A v9.8.7 CLI release on the stable manifest.
mkdir -p "$TMP/build"
for binary in phux phux-mcp; do printf '%s v9.8.7\n' "$binary" > "$TMP/build/$binary"; done
chmod 755 "$TMP/build/"*
bash "$ROOT/scripts/pack-release.sh" --id v9.8.7 --target "$TARGET" --bin-dir "$TMP/build" \
  --out-dir "$SERVED" >/dev/null 2>&1
jq --indent 2 --arg digest "$(sha "$SERVED/phux-v9.8.7-$TARGET.tar.gz")" \
  ".products.phux.artifacts[\"$TARGET\"].sha256 = \$digest" \
  "$ROOT/scripts/fixtures/release-manifest/stable.json" > "$SERVED/stable.json"
cp "$ROOT/scripts/fixtures/release-manifest/next.json" "$SERVED/next.json"
versions="$HOME/Library/Application Support/sh.phux/Server/Versions"

# A release without the asset still installs the CLI, and says so.
install_mac >/dev/null 2>"$TMP/err" || fail "install without a server asset: $(cat "$TMP/err")"
grep -Fq 'phux Server.app was not installed' "$TMP/err" || fail "no warning: $(cat "$TMP/err")"
grep -Fxq 'phux v9.8.7' "$TMP/cli/phux" && [[ ! -e $versions ]] || fail "CLI without bundle"

app="$TMP/src/phux Server.app"
mkdir -p "$app/Contents/MacOS"
printf 'server v9.8.7\n' > "$app/Contents/MacOS/phux"
chmod 755 "$app/Contents/MacOS/phux"
bash "$ROOT/scripts/make-server-app.sh" --pack "$app" "$SERVED" v9.8.7 "$TARGET" >/dev/null 2>&1
install_mac >/dev/null 2>"$TMP/err" || fail "install: $(cat "$TMP/err")"
[[ $(readlink "$versions/Current") == v9.8.7 ]] || fail "Current: $(ls -l "$versions")"
grep -Fxq 'server v9.8.7' "$versions/Current/phux Server.app/Contents/MacOS/phux" || fail "bundle binary"

# Older releases are pruned; the one Current named before is kept.
mkdir -p "$versions/v1.0.0" "$versions/v2.0.0"
ln -sfn v2.0.0 "$versions/Current"
install_mac >/dev/null 2>&1 || fail "reinstall"
[[ $(cd "$versions" && echo *) == 'Current v2.0.0 v9.8.7' ]] || fail "prune: $(cd "$versions" && echo *)"
[[ $(readlink "$versions/Current") == v9.8.7 ]] || fail "Current after reinstall"

# next publishes no bundle.
rm -rf "$HOME/Library"
install_mac --channel next >/dev/null 2>&1 || true
[[ ! -e $versions ]] || fail "next installed a bundle"

# A member outside the bundle is refused and nothing is installed.
stem="phux-server-v9.8.7-$TARGET"
mkdir -p "$TMP/evil/$stem"
printf 'x\n' > "$TMP/evil/$stem/extra"
tar -czf "$SERVED/$stem.tar.gz" -C "$TMP/evil" "$stem"
echo "$(sha "$SERVED/$stem.tar.gz")  $stem.tar.gz" > "$SERVED/$stem.tar.gz.sha256"
install_mac >/dev/null 2>"$TMP/err" || fail "install with a bad bundle"
grep -Fq 'unexpected archive member' "$TMP/err" || fail "bad member: $(cat "$TMP/err")"
[[ ! -e $versions/v9.8.7 ]] || fail "a bad bundle was installed"

echo "server app installer tests passed"
