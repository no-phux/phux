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
if [[ $src == */.phux-install.*/phux-mcp && $dst == "${FAIL_INSTALL_DIR:-/nonexistent}/phux-mcp" && ! -e $FAIL_MARKER ]]; then
  : > "$FAIL_MARKER"
  exit 1
fi
if [[ -n ${COCKPIT_MV_ACTION:-} && ! -e $COCKPIT_MV_MARKER ]]; then
  operation=""
  case "$dst" in
    */backup/'Phux Cockpit.app') operation=backup ;;
    "$COCKPIT_MV_APPS/Phux Cockpit.app") operation=publish ;;
  esac
  case "$COCKPIT_MV_ACTION" in
    "$operation-fail") : > "$COCKPIT_MV_MARKER"; exit 1 ;;
    "$operation-term")
      /bin/mv "$@"
      : > "$COCKPIT_MV_MARKER"
      kill -TERM "$PPID"
      exit 0
      ;;
  esac
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
  mkdir -p "$2/Contents"
  printf 'partial copy\n' > "$2/Contents/Info.plist"
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
  shift
  PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$FIXTURE" XATTR_MARKER="$TMP/xattr-cleared" \
    "$INSTALLER_SH" "$ROOT/scripts/install-cockpit.sh" --version "$COCKPIT_VERSION" \
      --os darwin --arch arm64 --applications-dir "$apps_dir" "$@"
}

COCKPIT_APPS="$TMP/cockpit-apps"
output="$(run_cockpit_install "$COCKPIT_APPS")"
grep -Fq "installed Phux Cockpit $COCKPIT_VERSION to $COCKPIT_APPS" <<<"$output"
grep -Fq 'next: open ' <<<"$output"
cmp "$FIXTURE/Phux Cockpit.app/Contents/Info.plist" "$COCKPIT_APPS/Phux Cockpit.app/Contents/Info.plist"
cmp "$FIXTURE/Phux Cockpit.app/Contents/MacOS/phux-cockpit" "$COCKPIT_APPS/Phux Cockpit.app/Contents/MacOS/phux-cockpit"
[[ -e $TMP/xattr-cleared ]] || {
  echo "cockpit installer did not clear the quarantine attribute" >&2
  exit 1
}

# Re-running the same install replaces the bundle as a unit: no nesting and no
# files left over from a previous version.
printf 'obsolete resource\n' > "$COCKPIT_APPS/Phux Cockpit.app/Contents/obsolete"
run_cockpit_install "$COCKPIT_APPS" > "$TMP/cockpit-repeat.out"
diff -r "$FIXTURE/Phux Cockpit.app" "$COCKPIT_APPS/Phux Cockpit.app"

# Execute only the printed shell command, with open stubbed. Name-based launch
# can select a different registered bundle; the installer must name its own
# absolute destination, with spaces and apostrophes preserved as one argument.
cat > "$FAKE_BIN/open" <<'EOF'
#!/bin/sh
printf '%s\n' "$@" > "$OPEN_ARGUMENTS"
EOF
chmod 755 "$FAKE_BIN/open"
CUSTOM_APPS="$TMP/someone's custom applications"
output="$(run_cockpit_install "$CUSTOM_APPS")"
next_command="$(sed -n 's/^next: //p' <<<"$output")"
PATH="$FAKE_BIN:/usr/bin:/bin" OPEN_ARGUMENTS="$TMP/open-arguments" \
  "$INSTALLER_SH" -c "$next_command"
printf '%s/Phux Cockpit.app\n' "$(cd "$CUSTOM_APPS" && pwd -P)" > "$TMP/expected-open-arguments"
cmp "$TMP/expected-open-arguments" "$TMP/open-arguments"

# Relative destinations are resolved from the install working directory, never
# from a competing CDPATH entry (whose cd also prints unsolicited stdout).
mkdir -p "$TMP/relative-cwd" "$TMP/cdpath/apps"
output="$(cd "$TMP/relative-cwd" && CDPATH="$TMP/cdpath" run_cockpit_install apps)"
next_command="$(sed -n 's/^next: //p' <<<"$output")"
PATH="$FAKE_BIN:/usr/bin:/bin" OPEN_ARGUMENTS="$TMP/open-arguments" \
  "$INSTALLER_SH" -c "$next_command"
printf '%s/Phux Cockpit.app\n' "$(cd "$TMP/relative-cwd/apps" && pwd -P)" > "$TMP/expected-open-arguments"
cmp "$TMP/expected-open-arguments" "$TMP/open-arguments"

# The active publisher owns its lock, even when another installer refuses it.
COCKPIT_LOCKED="$TMP/cockpit-locked"
mkdir -p "$COCKPIT_LOCKED/.phux-cockpit-install.lock"
if run_cockpit_install "$COCKPIT_LOCKED" >"$TMP/cockpit-locked.out" 2>"$TMP/cockpit-locked.err"; then
  echo 'cockpit installer ignored an active publish lock' >&2
  exit 1
fi
[[ -d $COCKPIT_LOCKED/.phux-cockpit-install.lock ]]
[[ ! -e "$COCKPIT_LOCKED/Phux Cockpit.app" ]]

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
if PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$FIXTURE" XATTR_MARKER="$TMP/xattr-unused" \
  FAIL_DITTO=1 FAIL_MARKER="$TMP/ditto-failed" \
  "$INSTALLER_SH" "$ROOT/scripts/install-cockpit.sh" --version "$COCKPIT_VERSION" \
    --os darwin --arch arm64 --applications-dir "$COCKPIT_ROLLBACK" >"$TMP/cockpit-rollback.out" 2>"$TMP/cockpit-rollback.err"; then
  echo "cockpit installer unexpectedly succeeded after forced placement failure" >&2
  exit 1
fi
[[ -e $TMP/ditto-failed ]] || { echo 'ditto failure injection was not reached' >&2; exit 1; }
grep -Fxq 'old plist' "$COCKPIT_ROLLBACK/Phux Cockpit.app/Contents/Info.plist"
if find "$COCKPIT_ROLLBACK" -maxdepth 1 -name '.phux-cockpit-install*' -print -quit | grep -q .; then
  echo "cockpit installer left transaction artifacts after rollback" >&2
  exit 1
fi

# A backup rename failure must leave the original app intact. A failure or
# signal at either rename must restore it, including a signal delivered after
# mv succeeded but before the shell executes its next statement.
for action in backup-fail backup-term publish-fail publish-term; do
  apps="$TMP/cockpit-$action"
  marker="$TMP/cockpit-$action-reached"
  mkdir -p "$apps/Phux Cockpit.app/Contents"
  printf 'old plist\n' > "$apps/Phux Cockpit.app/Contents/Info.plist"
  if COCKPIT_MV_ACTION="$action" COCKPIT_MV_MARKER="$marker" COCKPIT_MV_APPS="$apps" \
    run_cockpit_install "$apps" >"$TMP/$action.out" 2>"$TMP/$action.err"; then
    echo "cockpit installer unexpectedly succeeded after $action" >&2
    exit 1
  fi
  [[ -e $marker ]] || { echo "$action injection was not reached" >&2; exit 1; }
  grep -Fxq 'old plist' "$apps/Phux Cockpit.app/Contents/Info.plist"
  if find "$apps" -maxdepth 1 -name '.phux-cockpit-install*' -print -quit | grep -q .; then
    echo "cockpit installer left transaction artifacts after $action" >&2
    exit 1
  fi
done

# A first-time install has no backup. Failure or interruption during its publish
# rename must remove the new app rather than leave a partial/uncertain install.
for action in publish-fail publish-term; do
  apps="$TMP/cockpit-fresh-$action"
  marker="$TMP/cockpit-fresh-$action-reached"
  if COCKPIT_MV_ACTION="$action" COCKPIT_MV_MARKER="$marker" COCKPIT_MV_APPS="$apps" \
    run_cockpit_install "$apps" >"$TMP/fresh-$action.out" 2>"$TMP/fresh-$action.err"; then
    echo "fresh cockpit install unexpectedly succeeded after $action" >&2
    exit 1
  fi
  [[ -e $marker ]] || { echo "fresh $action injection was not reached" >&2; exit 1; }
  [[ ! -e "$apps/Phux Cockpit.app" ]]
  if find "$apps" -maxdepth 1 -name '.phux-cockpit-install*' -print -quit | grep -q .; then
    echo "fresh cockpit install left transaction artifacts after $action" >&2
    exit 1
  fi
done

echo "cockpit installer transaction tests passed"
