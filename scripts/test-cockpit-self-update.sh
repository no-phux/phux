#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

INSTALLER_SH="$(command -v dash || echo /bin/sh)"
DRIVER="$ROOT/scripts/cockpit-self-update.sh"
INSTALLER="$ROOT/scripts/install-cockpit.sh"

field() {
  local key=$1 file=$2
  awk -v key="$key" 'index($0, key ": ") == 1 { print substr($0, length(key) + 3); exit }' "$file"
}

make_bundle() {
  local dest=$1 version=$2 channel=${3:-} sha=${4:-}
  local channel_keys=""
  if [[ -n $channel ]]; then
    channel_keys="  <key>PhuxChannel</key>
  <string>${channel}</string>
  <key>PhuxBuildSHA</key>
  <string>${sha}</string>"
  fi
  mkdir -p "$dest/Contents/MacOS"
  cat > "$dest/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleShortVersionString</key>
  <string>${version}</string>
  <key>CFBundleIdentifier</key>
  <string>dev.phux.cockpit</string>
${channel_keys}
</dict>
</plist>
EOF
  printf 'old cockpit\n' > "$dest/Contents/MacOS/phux-cockpit"
  chmod 755 "$dest/Contents/MacOS/phux-cockpit"
}

FAKE_BIN="$TMP/fake-bin"
FIXTURE="$TMP/fixture"
mkdir -p "$FAKE_BIN" "$FIXTURE"

COCKPIT_VERSION=cockpit-v9.8.7
COCKPIT_SEMVER=9.8.7
COCKPIT_ZIP="phux-cockpit-${COCKPIT_SEMVER}-macos-arm64.zip"
mkdir -p "$FIXTURE/Phux Cockpit.app/Contents/MacOS"
printf 'new cockpit\n' > "$FIXTURE/Phux Cockpit.app/Contents/MacOS/phux-cockpit"
chmod 755 "$FIXTURE/Phux Cockpit.app/Contents/MacOS/phux-cockpit"
cat > "$FIXTURE/Phux Cockpit.app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleShortVersionString</key><string>${COCKPIT_SEMVER}</string>
</dict></plist>
EOF
(cd "$FIXTURE" && zip -qr "$TMP/$COCKPIT_ZIP" "Phux Cockpit.app" >/dev/null)
mv "$TMP/$COCKPIT_ZIP" "$FIXTURE/cockpit.zip"
(
  cd "$FIXTURE"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum cockpit.zip | sed "s/cockpit.zip\$/$COCKPIT_ZIP/" > cockpit.SHA256SUMS
  else
    shasum -a 256 cockpit.zip | sed "s/cockpit.zip\$/$COCKPIT_ZIP/" > cockpit.SHA256SUMS
  fi
)

NEXT_SHA=0123456789abcdef0123456789abcdef01234567
OLD_SHA=89abcdef0123456789abcdef0123456789abcdef
NEXT_ZIP="phux-cockpit-next.${NEXT_SHA}-macos-arm64.zip"
sed "s/$COCKPIT_ZIP\$/$NEXT_ZIP/" "$FIXTURE/cockpit.SHA256SUMS" > "$FIXTURE/next.sha256"
cat > "$FIXTURE/cockpit-channel.json" <<EOF
{"schema_version":1,"channel":"next","product":"cockpit","sha":"${NEXT_SHA}","version":"9.8.7"}
EOF

cat > "$FAKE_BIN/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
url=""
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -o) out=$2; shift 2 ;;
    -fsSL|-fsSLI|-f|-s|-S|-L|-I|-q) shift ;;
    -w) shift 2 ;;
    http*) url=$1; shift ;;
    *) shift ;;
  esac
done
case "$url" in
  */download/next/cockpit-channel.json)
    if [[ -n $out ]]; then cp "$INSTALL_FIXTURE/cockpit-channel.json" "$out"
    else cat "$INSTALL_FIXTURE/cockpit-channel.json"; fi
    ;;
  */download/next/phux-cockpit-next.*.zip.sha256) cp "$INSTALL_FIXTURE/next.sha256" "$out" ;;
  */download/next/phux-cockpit-next.*.zip) cp "$INSTALL_FIXTURE/cockpit.zip" "$out" ;;
  *SHA256SUMS)
    if [[ ${BAD_CHECKSUM:-0} == 1 ]]; then
      printf '0000000000000000000000000000000000000000000000000000000000000000  %s\n' \
        "phux-cockpit-9.8.7-macos-arm64.zip" > "$out"
    else
      cp "$INSTALL_FIXTURE/cockpit.SHA256SUMS" "$out"
    fi
    ;;
  *cockpit*.zip) cp "$INSTALL_FIXTURE/cockpit.zip" "$out" ;;
  *) exit 1 ;;
esac
EOF
chmod 755 "$FAKE_BIN/curl"

cat > "$FAKE_BIN/ditto" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${FAIL_DITTO:-0} == 1 ]]; then
  mkdir -p "$2/Contents"
  printf 'partial copy\n' > "$2/Contents/Info.plist"
  exit 1
fi
cp -Rf "$1" "$2"
EOF
chmod 755 "$FAKE_BIN/ditto"

cat > "$FAKE_BIN/xattr" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
exit 0
EOF
chmod 755 "$FAKE_BIN/xattr"

run_driver() {
  PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$FIXTURE" \
    "$INSTALLER_SH" "$DRIVER" --installer "$INSTALLER" --os darwin --arch arm64 \
    --homebrew-prefix "$TMP/empty-brew" --nix-store "$TMP/empty-nix" \
    --bin-dir "$TMP/bin" "$@"
}

APPS="$TMP/Applications"
mkdir -p "$APPS"
make_bundle "$APPS/Phux Cockpit.app" "0.1.0"

# --- current ---------------------------------------------------------------
run_driver --check --bundle "$APPS/Phux Cockpit.app" --home "$TMP" \
  --latest cockpit-v0.1.0 > "$TMP/current.out"
[[ $(field status "$TMP/current.out") == current ]]
[[ $(field current "$TMP/current.out") == 0.1.0 ]]
grep -Fq 'is current' "$TMP/current.out"

# --- newer -----------------------------------------------------------------
run_driver --check --bundle "$APPS/Phux Cockpit.app" --home "$TMP" \
  --latest "$COCKPIT_VERSION" > "$TMP/newer.out"
[[ $(field status "$TMP/newer.out") == newer ]]
[[ $(field latest "$TMP/newer.out") == "$COCKPIT_VERSION" ]]
grep -Fq 'is available' "$TMP/newer.out"

# --- Homebrew refusal ------------------------------------------------------
BREW="$TMP/brew"
mkdir -p "$BREW/Caskroom/phux-cockpit/0.1.0"
if run_driver --check --bundle "$APPS/Phux Cockpit.app" --home "$TMP" \
  --homebrew-prefix "$BREW" --latest "$COCKPIT_VERSION" > "$TMP/brew.out" 2>"$TMP/brew.err"; then
  echo "driver overwrote a Homebrew cask install" >&2
  exit 1
fi
[[ $(field status "$TMP/brew.out") == refused ]]
[[ $(field source "$TMP/brew.out") == homebrew ]]
grep -Fq 'brew upgrade --cask no-phux/tap/phux-cockpit' "$TMP/brew.out"
grep -Fxq 'old cockpit' "$APPS/Phux Cockpit.app/Contents/MacOS/phux-cockpit"

# --- Nix refusal -----------------------------------------------------------
NIX="$TMP/nix/store/hash-phux-cockpit/Phux Cockpit.app"
make_bundle "$NIX" "0.1.0"
if run_driver --check --bundle "$NIX" --home "$TMP" --nix-store "$TMP/nix/store" \
  --latest "$COCKPIT_VERSION" > "$TMP/nix.out"; then
  echo "driver offered to overwrite a Nix store path" >&2
  exit 1
fi
[[ $(field status "$TMP/nix.out") == refused ]]
[[ $(field source "$TMP/nix.out") == nix ]]

# --- unknown / source refusal ----------------------------------------------
SRC="$TMP/src/Phux Cockpit.app"
make_bundle "$SRC" "0.1.0"
if run_driver --check --bundle "$SRC" --home "$TMP" --latest "$COCKPIT_VERSION" \
  > "$TMP/unknown.out"; then
  echo "driver offered to overwrite an unrecognized layout" >&2
  exit 1
fi
[[ $(field status "$TMP/unknown.out") == refused ]]
[[ $(field source "$TMP/unknown.out") == unknown ]]
grep -Fq 'https://phux.sh/install-cockpit' "$TMP/unknown.out"

# --- failed checksum: original remains -------------------------------------
if PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$FIXTURE" BAD_CHECKSUM=1 \
  "$INSTALLER_SH" "$DRIVER" --install --installer "$INSTALLER" --os darwin --arch arm64 \
  --bundle "$APPS/Phux Cockpit.app" --home "$TMP" --latest "$COCKPIT_VERSION" \
  --homebrew-prefix "$TMP/empty-brew" --nix-store "$TMP/empty-nix" \
  --bin-dir "$TMP/bin" \
  > "$TMP/checksum.out" 2>"$TMP/checksum.err"; then
  echo "driver installed a release with a bad checksum" >&2
  exit 1
fi
[[ $(field status "$TMP/checksum.out") == failed ]] || grep -Fq 'error:' "$TMP/checksum.err"
grep -Fxq 'old cockpit' "$APPS/Phux Cockpit.app/Contents/MacOS/phux-cockpit"

# --- rollback on placement failure -----------------------------------------
if PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$FIXTURE" FAIL_DITTO=1 \
  "$INSTALLER_SH" "$DRIVER" --install --installer "$INSTALLER" --os darwin --arch arm64 \
  --bundle "$APPS/Phux Cockpit.app" --home "$TMP" --latest "$COCKPIT_VERSION" \
  --homebrew-prefix "$TMP/empty-brew" --nix-store "$TMP/empty-nix" \
  --bin-dir "$TMP/bin" \
  > "$TMP/rollback.out" 2>"$TMP/rollback.err"; then
  echo "driver reported success after a placement failure" >&2
  exit 1
fi
grep -Fxq 'old cockpit' "$APPS/Phux Cockpit.app/Contents/MacOS/phux-cockpit"
if find "$APPS" -maxdepth 1 -name '.phux-cockpit-install*' -print -quit | grep -q .; then
  echo "driver left installer transaction artifacts after rollback" >&2
  exit 1
fi

# --- install through install-cockpit.sh ------------------------------------
run_driver --install --bundle "$APPS/Phux Cockpit.app" --home "$TMP" \
  --latest "$COCKPIT_VERSION" > "$TMP/install.out"
[[ $(field status "$TMP/install.out") == installed ]]
[[ $(field relaunch "$TMP/install.out") == yes ]]
grep -Fxq 'new cockpit' "$APPS/Phux Cockpit.app/Contents/MacOS/phux-cockpit"

# --- channels ----------------------------------------------------------------
# A stable bundle asked for next is a switch, whatever the versions say.
make_bundle "$APPS/Phux Cockpit.app" "9.8.7"
run_driver --check --channel next --bundle "$APPS/Phux Cockpit.app" --home "$TMP" \
  --latest "next.${NEXT_SHA}" > "$TMP/switch.out"
[[ $(field status "$TMP/switch.out") == newer ]]
[[ $(field channel "$TMP/switch.out") == next ]]
[[ $(field latest "$TMP/switch.out") == 9.8.7+next.0123456 ]]
grep -Fq 'Switching Phux Cockpit to the next channel' "$TMP/switch.out"

# A next bundle follows next on its own and compares SHAs, not versions.
make_bundle "$APPS/Phux Cockpit.app" "9.8.7" next "$NEXT_SHA"
run_driver --check --bundle "$APPS/Phux Cockpit.app" --home "$TMP" \
  --latest "next.${NEXT_SHA}" > "$TMP/next-current.out"
[[ $(field status "$TMP/next-current.out") == current ]]
[[ $(field channel "$TMP/next-current.out") == next ]]
[[ $(field current "$TMP/next-current.out") == 9.8.7+next.0123456 ]]
make_bundle "$APPS/Phux Cockpit.app" "9.8.7" next "$OLD_SHA"
run_driver --check --bundle "$APPS/Phux Cockpit.app" --home "$TMP" \
  --latest "next.${NEXT_SHA}" > "$TMP/next-newer.out"
[[ $(field status "$TMP/next-newer.out") == newer ]]
[[ $(field current "$TMP/next-newer.out") == 9.8.7+next.89abcde ]]

# Back to stable at the same version is still a switch.
run_driver --check --channel latest --bundle "$APPS/Phux Cockpit.app" --home "$TMP" \
  --latest "$COCKPIT_VERSION" > "$TMP/to-stable.out"
[[ $(field status "$TMP/to-stable.out") == newer ]]
[[ $(field channel "$TMP/to-stable.out") == stable ]]

# End to end: resolve the pointer through the installer and install next.
run_driver --install --channel next --bundle "$APPS/Phux Cockpit.app" --home "$TMP" \
  > "$TMP/next-install.out"
[[ $(field status "$TMP/next-install.out") == installed ]]
[[ $(field latest "$TMP/next-install.out") == 9.8.7+next.0123456 ]]
grep -Fxq 'new cockpit' "$APPS/Phux Cockpit.app/Contents/MacOS/phux-cockpit"

# The next archive is verified against its own sidecar before placement.
make_bundle "$APPS/Phux Cockpit.app" "9.8.7"
printf '0000000000000000000000000000000000000000000000000000000000000000  %s\n' \
  "$NEXT_ZIP" > "$FIXTURE/next.sha256"
if run_driver --install --channel next --bundle "$APPS/Phux Cockpit.app" --home "$TMP" \
  > "$TMP/next-bad.out" 2>"$TMP/next-bad.err"; then
  echo "driver installed a next build with a bad checksum" >&2
  exit 1
fi
grep -Fxq 'old cockpit' "$APPS/Phux Cockpit.app/Contents/MacOS/phux-cockpit"

# Prove the driver invoked the real installer rather than unpacking itself.
grep -Fq 'install-cockpit.sh' "$DRIVER"

echo "cockpit self-update tests passed"
