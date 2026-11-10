#!/usr/bin/env bash
# Assemble the macOS server bundle, `phux Server.app`, from a phux binary:
#
#   scripts/make-server-app.sh PHUX_BINARY OUT_DIR VERSION
#   scripts/make-server-app.sh --pack APP OUT_DIR ID TARGET
#
# The first form writes OUT_DIR/phux Server.app: Contents/MacOS/phux is the
# binary itself, Info.plist comes from crates/phux/packaging/server with
# VERSION (X.Y.Z, a prerelease suffix allowed) filled in, and the icon is
# Rune's. On macOS it is sealed ad hoc, so even an unsigned build carries
# the sh.phux.server identifier; release CI re-signs it with the Developer
# ID (scripts/ci/sign-macos-desktop.sh). The second form packs a finished
# bundle into OUT_DIR/phux-server-ID-TARGET.tar.gz plus its .sha256, the
# asset the installer and `phux update` install beside the CLI.
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
app_name="phux Server.app"

die() {
  phux_error "$*"
  exit 1
}

usage() {
  sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

pack() {
  local app="$1" out="$2" id="$3" target="$4"
  case "$id$target" in */* | *..*) die "ID and TARGET must not contain a path" ;; esac
  [[ -x "$app/Contents/MacOS/phux" ]] || die "not a server bundle: $app"
  mkdir -p "$out"
  out="$(cd "$out" && pwd)"
  local stem="phux-server-${id}-${target}" stage
  stage="$(mktemp -d)"
  # The bundle is copied with ditto where it exists so its signature and
  # stapled ticket travel intact; no AppleDouble files enter the archive.
  mkdir "$stage/$stem"
  if command -v ditto >/dev/null 2>&1; then
    ditto --norsrc "$app" "$stage/$stem/$app_name"
  else
    cp -R "$app" "$stage/$stem/$app_name"
  fi
  COPYFILE_DISABLE=1 tar -czf "$out/$stem.tar.gz" -C "$stage" "$stem"
  rm -rf "$stage"
  local sha
  if command -v sha256sum >/dev/null 2>&1; then
    sha="$(sha256sum "$out/$stem.tar.gz" | cut -d' ' -f1)"
  else
    sha="$(shasum -a 256 "$out/$stem.tar.gz" | cut -d' ' -f1)"
  fi
  echo "${sha}  ${stem}.tar.gz" > "$out/$stem.tar.gz.sha256"
  phux_ok "packaged $out/$stem.tar.gz"
}

if [[ "${1:-}" == --pack ]]; then
  [[ $# -eq 5 ]] || usage
  pack "$2" "$3" "$4" "$5"
  exit 0
fi
[[ $# -eq 3 ]] || usage
binary="$1" out="$2" version="$3"
[[ -f "$binary" && -x "$binary" ]] || die "not an executable: $binary"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || die "VERSION must be X.Y.Z: $version"

mkdir -p "$out"
app="$(cd "$out" && pwd)/$app_name"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/phux"
chmod 755 "$app/Contents/MacOS/phux"
# CFBundleVersion takes only dot-separated integers: drop a prerelease.
sed -e "s/@VERSION@/$version/g" -e "s/@BUILD@/${version%%-*}/g" \
  "$root/crates/phux/packaging/server/Info.plist" > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"

if [[ "$(uname -s)" == Darwin ]]; then
  iconset="$(mktemp -d)/AppIcon.iconset"
  mkdir -p "$iconset"
  for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$root/clients/rune/packaging/AppIcon.png" \
      --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size * 2))" "$((size * 2))" "$root/clients/rune/packaging/AppIcon.png" \
      --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
  rm -rf "$(dirname "$iconset")"
  codesign --force --sign - --timestamp=none "$app"
  codesign --verify --strict "$app"
fi
phux_ok "assembled $app"
