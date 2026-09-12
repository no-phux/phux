#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# The installer is served at https://phux.sh/install and users pipe it to `sh`,
# so every case below drives it through a real POSIX shell rather than bash.
# Prefer dash where the host has it: macOS `/bin/sh` is bash in POSIX mode and
# forgives most bashisms, while Debian and Ubuntu — including the CI runners —
# resolve `/bin/sh` to dash, which does not.
INSTALLER_SH="$(command -v dash || echo /bin/sh)"
echo "installer shell: $INSTALLER_SH"

VERSION=v9.8.7
TARGET=x86_64-unknown-linux-gnu
STAGE="phux-${VERSION}-${TARGET}"
FIXTURE="$TMP/fixture"
FAKE_BIN="$TMP/fake-bin"
mkdir -p "$FIXTURE/$STAGE" "$FAKE_BIN"
printf 'new phux\n' > "$FIXTURE/$STAGE/phux"
printf 'new phux-mcp\n' > "$FIXTURE/$STAGE/phux-mcp"
chmod 755 "$FIXTURE/$STAGE/phux" "$FIXTURE/$STAGE/phux-mcp"
tar -czf "$FIXTURE/$STAGE.tar.gz" -C "$FIXTURE" "$STAGE"
(
  cd "$FIXTURE"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$STAGE.tar.gz" > "$STAGE.tar.gz.sha256"
  else
    shasum -a 256 "$STAGE.tar.gz" > "$STAGE.tar.gz.sha256"
  fi
)

cat > "$FAKE_BIN/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
url=$2
out=$4
case "$url" in
  *cockpit*.zip) cp "$INSTALL_FIXTURE/cockpit.zip" "$out" ;;
  *cockpit*SHA256SUMS) cp "$INSTALL_FIXTURE/cockpit.SHA256SUMS" "$out" ;;
  *.sha256) cp "$INSTALL_FIXTURE/phux-v9.8.7-x86_64-unknown-linux-gnu.tar.gz.sha256" "$out" ;;
  *) cp "$INSTALL_FIXTURE/phux-v9.8.7-x86_64-unknown-linux-gnu.tar.gz" "$out" ;;
esac
EOF
chmod 755 "$FAKE_BIN/curl"

run_install() {
  local install_dir=$1
  local path=$2
  PATH="$FAKE_BIN:$path" INSTALL_FIXTURE="$FIXTURE" \
    "$INSTALLER_SH" "$ROOT/scripts/install.sh" --version "$VERSION" --os linux --arch x86_64 \
      --install-dir "$install_dir"
}

ON_PATH="$TMP/on-path"
mkdir "$ON_PATH"
ON_PATH_CANON="$(cd "$ON_PATH" && pwd -P)"
output="$(run_install "$ON_PATH" "$ON_PATH:/usr/bin:/bin")"
grep -Fq 'next: phux' <<<"$output"
if grep -Fq 'PATH remedy:' <<<"$output"; then
  echo "installer printed a PATH remedy for a discoverable destination" >&2
  exit 1
fi
cmp "$FIXTURE/$STAGE/phux" "$ON_PATH/phux"
cmp "$FIXTURE/$STAGE/phux-mcp" "$ON_PATH/phux-mcp"

SHADOW="$TMP/shadow"
mkdir "$SHADOW"
printf '#!/bin/sh\nexit 0\n' > "$SHADOW/phux"
chmod 755 "$SHADOW/phux"
output="$(run_install "$ON_PATH" "$SHADOW:$ON_PATH:/usr/bin:/bin")"
grep -Fq "next: $ON_PATH_CANON/phux" <<<"$output"
grep -Fq 'PATH remedy:' <<<"$output"

OFF_PATH="$TMP/off-path"
output="$(run_install "$OFF_PATH" "/usr/bin:/bin")"
OFF_PATH_CANON="$(cd "$OFF_PATH" && pwd -P)"
grep -Fq "next: $OFF_PATH_CANON/phux" <<<"$output"
grep -Fq "PATH remedy: export PATH=$OFF_PATH_CANON:\"\$PATH\"" <<<"$output"

# Fail only the second publish rename. The EXIT trap must restore both old
# binaries and remove its lock and transaction directory.
cat > "$FAKE_BIN/mv" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
src=${@: -2:1}
dst=${@: -1}
if [[ ${SIGNAL_AFTER_FIRST_PUBLISH:-0} == 1 && $src == */.phux-install.*/phux && $dst == "$SIGNAL_INSTALL_DIR/phux" && ! -e $SIGNAL_MARKER ]]; then
  /bin/mv "$@"
  : > "$SIGNAL_MARKER"
  kill -TERM "$PPID"
  exit 0
fi
if [[ $src == */.phux-install.*/phux-mcp && $dst == "$FAIL_INSTALL_DIR/phux-mcp" && ! -e $FAIL_MARKER ]]; then
  : > "$FAIL_MARKER"
  exit 1
fi
exec /bin/mv "$@"
EOF
chmod 755 "$FAKE_BIN/mv"

ROLLBACK="$TMP/rollback"
mkdir "$ROLLBACK"
printf 'old phux\n' > "$ROLLBACK/phux"
printf 'old phux-mcp\n' > "$ROLLBACK/phux-mcp"
if PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$FIXTURE" \
  FAIL_INSTALL_DIR="$ROLLBACK" FAIL_MARKER="$TMP/failed-once" \
  "$INSTALLER_SH" "$ROOT/scripts/install.sh" --version "$VERSION" --os linux --arch x86_64 \
    --install-dir "$ROLLBACK" >"$TMP/rollback.out" 2>"$TMP/rollback.err"; then
  echo "installer unexpectedly succeeded after forced second publish failure" >&2
  exit 1
fi
grep -Fxq 'old phux' "$ROLLBACK/phux"
grep -Fxq 'old phux-mcp' "$ROLLBACK/phux-mcp"
if find "$ROLLBACK" -maxdepth 1 -name '.phux-install*' -print -quit | grep -q .; then
  echo "installer left transaction artifacts after rollback" >&2
  exit 1
fi

# Interrupt a fresh install immediately after the first publish rename. The
# destination must contain the complete pair or neither binary, never half.
INTERRUPTED="$TMP/interrupted"
mkdir "$INTERRUPTED"
if PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$FIXTURE" \
  SIGNAL_AFTER_FIRST_PUBLISH=1 SIGNAL_INSTALL_DIR="$INTERRUPTED" \
  SIGNAL_MARKER="$TMP/signaled-once" FAIL_INSTALL_DIR=/nonexistent \
  FAIL_MARKER="$TMP/not-failed" \
  "$INSTALLER_SH" "$ROOT/scripts/install.sh" --version "$VERSION" --os linux --arch x86_64 \
    --install-dir "$INTERRUPTED" >"$TMP/interrupted.out" 2>"$TMP/interrupted.err"; then
  echo "installer unexpectedly succeeded after a publish interruption" >&2
  exit 1
fi
if [[ -e $INTERRUPTED/phux || -e $INTERRUPTED/phux-mcp ]]; then
  echo "installer left a partial binary pair after interruption" >&2
  exit 1
fi
if find "$INTERRUPTED" -maxdepth 1 -name '.phux-install*' -print -quit | grep -q .; then
  echo "installer left transaction artifacts after interruption" >&2
  exit 1
fi

# Refusing a concurrent install must never remove the active installer's lock.
LOCKED="$TMP/locked"
mkdir -p "$LOCKED/.phux-install.lock"
if run_install "$LOCKED" "/usr/bin:/bin" >"$TMP/locked.out" 2>"$TMP/locked.err"; then
  echo "installer unexpectedly ignored an active publish lock" >&2
  exit 1
fi
[[ -d $LOCKED/.phux-install.lock ]] || {
  echo "refused installer removed another installer's lock" >&2
  exit 1
}

echo "installer transaction tests passed"

# --- Cockpit installer -------------------------------------------------------
#
# Same discipline as above: the script is served at /install-cockpit and piped
# to `sh`, so it runs under dash here. macOS-only tools it needs (ditto for
# bundle placement, xattr for quarantine) are faked; unzip is real on both
# macOS and the Linux CI runners.
COCKPIT_VERSION=cockpit-v9.8.7
COCKPIT_SEMVER=9.8.7
COCKPIT_ZIP="phux-cockpit-${COCKPIT_SEMVER}-macos-arm64.zip"
mkdir -p "$FIXTURE/Phux Cockpit.app/Contents/MacOS"
printf 'cockpit plist\n' > "$FIXTURE/Phux Cockpit.app/Contents/Info.plist"
printf 'cockpit binary\n' > "$FIXTURE/Phux Cockpit.app/Contents/MacOS/phux-cockpit"
chmod 755 "$FIXTURE/Phux Cockpit.app/Contents/MacOS/phux-cockpit"
(cd "$FIXTURE" && zip -qr "$TMP/$COCKPIT_ZIP" "Phux Cockpit.app" >/dev/null)
mv "$TMP/$COCKPIT_ZIP" "$FIXTURE/cockpit.zip"
(
  cd "$FIXTURE"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum cockpit.zip | sed "s/cockpit.zip\$/$COCKPIT_ZIP/" > cockpit.SHA256SUMS
  else
    shasum -a 256 cockpit.zip | sed "s/cockpit.zip\$/$COCKPIT_ZIP/" > cockpit.SHA256SUMS
  fi
  # A dmg line for an asset that is never downloaded: verification must filter
  # to the zip, not demand the whole file.
  echo "0000000000000000000000000000000000000000000000000000000000000000  phux-cockpit-${COCKPIT_SEMVER}-macos-arm64.dmg" >> cockpit.SHA256SUMS
)

cat > "$FAKE_BIN/ditto" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${FAIL_DITTO:-0} == 1 && ! -e ${FAIL_MARKER:-/nonexistent-marker} ]]; then
  : > "$FAIL_MARKER"
  exit 1
fi
cp -Rf "$1" "$2"
EOF
chmod 755 "$FAKE_BIN/ditto"

cat > "$FAKE_BIN/xattr" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
: > "${XATTR_MARKER:-/dev/null}"
EOF
chmod 755 "$FAKE_BIN/xattr"

run_cockpit_install() {
  local apps_dir=$1
  local extra=${2:-}
  PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$FIXTURE" XATTR_MARKER="$TMP/xattr-cleared" \
    "$INSTALLER_SH" "$ROOT/scripts/install-cockpit.sh" --version "$COCKPIT_VERSION" \
      --os darwin --arch arm64 --applications-dir "$apps_dir" $extra
}

COCKPIT_APPS="$TMP/cockpit-apps"
output="$(run_cockpit_install "$COCKPIT_APPS")"
grep -Fq "installed Phux Cockpit $COCKPIT_VERSION to $COCKPIT_APPS" <<<"$output"
grep -Fq 'next: open -a "Phux Cockpit"' <<<"$output"
cmp "$FIXTURE/Phux Cockpit.app/Contents/Info.plist" "$COCKPIT_APPS/Phux Cockpit.app/Contents/Info.plist"
cmp "$FIXTURE/Phux Cockpit.app/Contents/MacOS/phux-cockpit" "$COCKPIT_APPS/Phux Cockpit.app/Contents/MacOS/phux-cockpit"
[[ -e $TMP/xattr-cleared ]] || {
  echo "cockpit installer did not clear the quarantine attribute" >&2
  exit 1
}

# A bare semver normalizes to the release tag.
output="$(PATH="$FAKE_BIN:/usr/bin:/bin" \
  "$INSTALLER_SH" "$ROOT/scripts/install-cockpit.sh" --version "$COCKPIT_SEMVER" \
    --os darwin --arch arm64 --applications-dir "$TMP/unused" --dry-run)"
grep -Fq "tag: $COCKPIT_VERSION" <<<"$output"
grep -Fq "zip_url: https://github.com/no-phux/phux/releases/download/${COCKPIT_VERSION}/${COCKPIT_ZIP}" <<<"$output"

# A failed placement restores the previous install and leaves no lock behind.
COCKPIT_ROLLBACK="$TMP/cockpit-rollback"
mkdir -p "$COCKPIT_ROLLBACK/Phux Cockpit.app/Contents"
printf 'old plist\n' > "$COCKPIT_ROLLBACK/Phux Cockpit.app/Contents/Info.plist"
if PATH="$FAKE_BIN:/usr/bin:/bin" XATTR_MARKER="$TMP/xattr-unused" \
  FAIL_DITTO=1 FAIL_MARKER="$TMP/ditto-failed" \
  "$INSTALLER_SH" "$ROOT/scripts/install-cockpit.sh" --version "$COCKPIT_VERSION" \
    --os darwin --arch arm64 --applications-dir "$COCKPIT_ROLLBACK" >"$TMP/cockpit-rollback.out" 2>"$TMP/cockpit-rollback.err"; then
  echo "cockpit installer unexpectedly succeeded after forced placement failure" >&2
  exit 1
fi
grep -Fxq 'old plist' "$COCKPIT_ROLLBACK/Phux Cockpit.app/Contents/Info.plist"
if find "$COCKPIT_ROLLBACK" -maxdepth 1 -name '.phux-cockpit-install*' -print -quit | grep -q .; then
  echo "cockpit installer left transaction artifacts after rollback" >&2
  exit 1
fi

echo "cockpit installer transaction tests passed"

# The rollback tests leave a wrapping `mv` in FAKE_BIN. The next-channel
# path uses a real rename and must not inherit that harness.
rm -f "$FAKE_BIN/mv"

NEXT_SHA=0123456789abcdef0123456789abcdef01234567
NEXT_STAGE="phux-next.${NEXT_SHA}-${TARGET}"
NEXT_FIXTURE="$TMP/next-fixture"
mkdir -p "$NEXT_FIXTURE/$NEXT_STAGE" "$NEXT_FIXTURE"
printf 'next phux\n' > "$NEXT_FIXTURE/$NEXT_STAGE/phux"
printf 'next phux-mcp\n' > "$NEXT_FIXTURE/$NEXT_STAGE/phux-mcp"
chmod 755 "$NEXT_FIXTURE/$NEXT_STAGE/phux" "$NEXT_FIXTURE/$NEXT_STAGE/phux-mcp"
tar -czf "$NEXT_FIXTURE/${NEXT_STAGE}.tar.gz" -C "$NEXT_FIXTURE" "$NEXT_STAGE"
(
  cd "$NEXT_FIXTURE"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${NEXT_STAGE}.tar.gz" > "${NEXT_STAGE}.tar.gz.sha256"
  else
    shasum -a 256 "${NEXT_STAGE}.tar.gz" > "${NEXT_STAGE}.tar.gz.sha256"
  fi
)
cat > "$NEXT_FIXTURE/channel.json" <<EOF
{"schema_version":1,"channel":"next","sha":"${NEXT_SHA}","version":"9.8.7"}
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
payload() {
  case "$url" in
    *channel.json) cat "$INSTALL_FIXTURE/channel.json" ;;
    *.sha256) cat "$INSTALL_FIXTURE/phux-next.0123456789abcdef0123456789abcdef01234567-x86_64-unknown-linux-gnu.tar.gz.sha256" ;;
    *next.*) cat "$INSTALL_FIXTURE/phux-next.0123456789abcdef0123456789abcdef01234567-x86_64-unknown-linux-gnu.tar.gz" ;;
    *) echo "unexpected url: $url" >&2; exit 1 ;;
  esac
}
if [[ -n "$out" ]]; then
  payload > "$out"
else
  payload
fi
EOF
chmod 755 "$FAKE_BIN/curl"

NEXT_DIR="$TMP/next-install"
mkdir "$NEXT_DIR"
PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$NEXT_FIXTURE" \
  "$INSTALLER_SH" "$ROOT/scripts/install.sh" --channel next --os linux --arch x86_64 \
    --install-dir "$NEXT_DIR" >"$TMP/next.out"
grep -Fq "installed phux next.${NEXT_SHA}" "$TMP/next.out"
grep -Fxq next "$NEXT_DIR/.phux-channel"
cmp "$NEXT_FIXTURE/$NEXT_STAGE/phux" "$NEXT_DIR/phux"
cmp "$NEXT_FIXTURE/$NEXT_STAGE/phux-mcp" "$NEXT_DIR/phux-mcp"

if PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$NEXT_FIXTURE" \
  "$INSTALLER_SH" "$ROOT/scripts/install.sh" --channel next --version "$VERSION" \
    --os linux --arch x86_64 --install-dir "$NEXT_DIR" \
    >"$TMP/next-conflict.out" 2>"$TMP/next-conflict.err"; then
  echo "installer accepted --channel next with --version" >&2
  exit 1
fi
grep -Fq -- '--version pins a stable tag' "$TMP/next-conflict.err"

echo "next-channel installer tests passed"
