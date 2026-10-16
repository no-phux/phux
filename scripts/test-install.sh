#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
# The installer edits a shell profile when its directory is off PATH; never
# let a test run touch the real one.
export HOME="$TMP/home" SHELL=/bin/zsh
unset ZDOTDIR XDG_CONFIG_HOME PHUX_NO_MODIFY_PATH
mkdir -p "$HOME"

# The installer is served at https://phux.sh/install and users pipe it to `sh`,
# so every case below drives it through a real POSIX shell rather than bash.
# Prefer dash where the host has it: macOS `/bin/sh` is bash in POSIX mode and
# forgives most bashisms, while Debian and Ubuntu — including the CI runners —
# resolve `/bin/sh` to dash, which does not.
INSTALLER_SH="$(command -v dash || echo /bin/sh)"
echo "installer shell: $INSTALLER_SH"

VERSION=v9.8.7
TARGET=x86_64-unknown-linux-gnu
BINS="$TMP/bins"
FIXTURE="$TMP/fixture"
FAKE_BIN="$TMP/fake-bin"
mkdir -p "$BINS" "$FIXTURE" "$FAKE_BIN"
printf 'new phux\n' > "$BINS/phux"
printf 'new phux-mcp\n' > "$BINS/phux-mcp"
chmod 755 "$BINS/phux" "$BINS/phux-mcp"
bash "$ROOT/scripts/pack-release.sh" \
  --id "$VERSION" --target "$TARGET" --bin-dir "$BINS" --out-dir "$FIXTURE"

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
      --install-dir "$install_dir" "${@:3}"
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
cmp "$BINS/phux" "$ON_PATH/phux"
cmp "$BINS/phux-mcp" "$ON_PATH/phux-mcp"

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
grep -Fq "added $OFF_PATH_CANON to PATH in $HOME/.zshrc" <<<"$output"
grep -Fxq "export PATH=$OFF_PATH_CANON:\"\$PATH\"" "$HOME/.zshrc"
# A rerun leaves exactly one profile line; an on-PATH or shadowed install
# never edits the profile at all.
run_install "$OFF_PATH" "/usr/bin:/bin" >/dev/null
[[ $(grep -c 'PATH=' "$HOME/.zshrc") -eq 1 ]]
SHELL=/usr/bin/fish run_install "$OFF_PATH" "/usr/bin:/bin" >/dev/null
grep -Fxq "fish_add_path -g $OFF_PATH_CANON" "$HOME/.config/fish/conf.d/phux.fish"
rm -rf "$HOME/.zshrc" "$HOME/.config"
run_install "$ON_PATH" "$ON_PATH:/usr/bin:/bin" >/dev/null
run_install "$ON_PATH" "$SHADOW:$ON_PATH:/usr/bin:/bin" >/dev/null
[[ ! -e $HOME/.zshrc ]] || { echo "installer edited a profile for an on-PATH directory" >&2; exit 1; }
output="$(PHUX_NO_MODIFY_PATH=1 run_install "$OFF_PATH" "/usr/bin:/bin")"
grep -Fq 'PATH remedy:' <<<"$output"
output="$(run_install "$OFF_PATH" "/usr/bin:/bin" --no-modify-path)"
grep -Fq 'PATH remedy:' <<<"$output"
[[ ! -e $HOME/.zshrc ]] || { echo "installer edited a profile despite opting out" >&2; exit 1; }

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
  kill -"${SIGNAL_NAME:-TERM}" "$PPID"
  exit 0
fi
if [[ $src == */.phux-install.*/phux-mcp && $dst == "${FAIL_INSTALL_DIR:-/nonexistent}/phux-mcp" && ! -e $FAIL_MARKER ]]; then
  : > "$FAIL_MARKER"
  exit 1
fi
exec /bin/mv "$@"
EOF
chmod 755 "$FAKE_BIN/mv"

cat > "$FAKE_BIN/mkdir" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ ${SIGNAL_AFTER_LOCK_MKDIR:-0} == 1 && ${1:-} != -* && ! -e ${SIGNAL_MARKER:-/nonexistent} ]]; then
  case "$1" in
    */.phux-install.lock.*)
      /bin/mkdir "$@"
      : > "$SIGNAL_MARKER"
      kill -"${SIGNAL_NAME:-TERM}" "$PPID"
      exit 0
      ;;
  esac
fi
exec /bin/mkdir "$@"
EOF
chmod 755 "$FAKE_BIN/mkdir"

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
for signal in TERM HUP; do
  INTERRUPTED="$TMP/interrupted-$signal"
  mkdir "$INTERRUPTED"
  marker="$TMP/signaled-once-$signal"
  if PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$FIXTURE" \
    SIGNAL_AFTER_FIRST_PUBLISH=1 SIGNAL_NAME="$signal" SIGNAL_INSTALL_DIR="$INTERRUPTED" \
    SIGNAL_MARKER="$marker" FAIL_INSTALL_DIR=/nonexistent \
    FAIL_MARKER="$TMP/not-failed-$signal" \
    "$INSTALLER_SH" "$ROOT/scripts/install.sh" --version "$VERSION" --os linux --arch x86_64 \
      --install-dir "$INTERRUPTED" >"$TMP/interrupted-$signal.out" 2>"$TMP/interrupted-$signal.err"; then
    echo "installer unexpectedly succeeded after a $signal publish interruption" >&2
    exit 1
  fi
  [[ -e $marker ]] || { echo "$signal publish interruption was not reached" >&2; exit 1; }
  if [[ -e $INTERRUPTED/phux || -e $INTERRUPTED/phux-mcp ]]; then
    echo "installer left a partial binary pair after $signal interruption" >&2
    exit 1
  fi
  if find "$INTERRUPTED" -maxdepth 1 -name '.phux-install*' -print -quit | grep -q .; then
    echo "installer left transaction artifacts after $signal interruption" >&2
    exit 1
  fi
done

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

# A signal after the lock mkdir succeeds, before lock_acquired=1, must not
# strand the lock. A later install has to be able to proceed.
for signal in TERM HUP; do
  dest="$TMP/lock-interrupt-$signal"
  marker="$TMP/lock-interrupt-$signal-reached"
  mkdir "$dest"
  if PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$FIXTURE" \
    SIGNAL_AFTER_LOCK_MKDIR=1 SIGNAL_NAME="$signal" SIGNAL_MARKER="$marker" \
    "$INSTALLER_SH" "$ROOT/scripts/install.sh" --version "$VERSION" --os linux --arch x86_64 \
      --install-dir "$dest" >"$TMP/lock-interrupt-$signal.out" 2>"$TMP/lock-interrupt-$signal.err"; then
    echo "installer unexpectedly succeeded after a $signal lock interruption" >&2
    exit 1
  fi
  [[ -e $marker ]] || { echo "$signal lock interruption was not reached" >&2; exit 1; }
  if find "$dest" -maxdepth 1 -name '.phux-install*' -print -quit | grep -q .; then
    echo "installer stranded a lock after $signal lock interruption" >&2
    exit 1
  fi
  run_install "$dest" "/usr/bin:/bin" >/dev/null
  cmp "$BINS/phux" "$dest/phux"
  cmp "$BINS/phux-mcp" "$dest/phux-mcp"
done

echo "installer transaction tests passed"

# The rollback tests leave a wrapping `mv` in FAKE_BIN. The next-channel
# path uses a real rename and must not inherit that harness.
rm -f "$FAKE_BIN/mv"

# Unpinned installs resolve from the channel manifest (ADR-0175) and verify
# against its digest; the sidecar is never fetched. The fixture is the golden
# manifest with this run's real artifact digest, re-rendered in the
# generator's exact form.
write_manifest() {
  local channel=$1 out=$2 sha=$3 digest=$4
  python3 - "$ROOT/scripts/fixtures/release-manifest/$channel.json" "$out" "$sha" "$digest" "$TARGET" <<'PY'
import json, sys
golden, out, sha, digest, target = sys.argv[1:]
m = json.load(open(golden))
phux = m["products"]["phux"]
phux["sha"] = sha
artifact = phux["artifacts"][target]
if m["channel"] == "next":
    artifact["name"] = f"phux-next.{sha}-{target}.tar.gz"
    artifact["url"] = f"https://github.com/no-phux/phux/releases/download/next/{artifact['name']}"
artifact["sha256"] = digest
open(out, "w").write(json.dumps(m, indent=2) + "\n")
PY
}

cat > "$FAKE_BIN/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
url=""
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -o) out=$2; shift 2 ;;
    http*) url=$1; shift ;;
    *) shift ;;
  esac
done
printf '%s\n' "$url" >> "$INSTALL_FIXTURE/calls"
case "$url" in
  https://github.com/no-phux/phux/releases/download/channels/*.json) file="$INSTALL_FIXTURE/${url##*/}" ;;
  *.tar.gz) file="$INSTALL_FIXTURE/${url##*/}" ;;
  *) echo "unexpected url: $url" >&2; exit 22 ;;
esac
[[ -f $file ]] || exit 22
if [[ -n "$out" ]]; then cp "$file" "$out"; else cat "$file"; fi
EOF
chmod 755 "$FAKE_BIN/curl"

digest_of() { awk '{ print $1 }' "$1.sha256"; }

STABLE_DIR="$TMP/stable-install"
mkdir "$STABLE_DIR"
STABLE_ARCHIVE="$FIXTURE/phux-${VERSION}-${TARGET}.tar.gz"
write_manifest stable "$FIXTURE/stable.json" "$(printf 'a%.0s' {1..40})" "$(digest_of "$STABLE_ARCHIVE")"
: > "$FIXTURE/calls"
PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$FIXTURE" \
  "$INSTALLER_SH" "$ROOT/scripts/install.sh" --os linux --arch x86_64 \
    --install-dir "$STABLE_DIR" >"$TMP/stable.out" 2>"$TMP/stable.err"
grep -Fq "phux: ok: installed phux ${VERSION}" "$TMP/stable.err"
grep -Fxq stable "$STABLE_DIR/.phux-channel"
cmp "$BINS/phux" "$STABLE_DIR/phux"
if grep -Fq '.sha256' "$FIXTURE/calls"; then echo "manifest install fetched a sidecar" >&2; exit 1; fi

# A digest the archive does not hash to installs nothing and keeps the old pair.
write_manifest stable "$FIXTURE/stable.json" "$(printf 'a%.0s' {1..40})" "$(printf '0%.0s' {1..64})"
printf 'old phux\n' > "$STABLE_DIR/phux"
if PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$FIXTURE" \
  "$INSTALLER_SH" "$ROOT/scripts/install.sh" --os linux --arch x86_64 \
    --install-dir "$STABLE_DIR" >"$TMP/tampered.out" 2>"$TMP/tampered.err"; then
  echo "installer accepted an archive that does not match the manifest digest" >&2
  exit 1
fi
grep -Fq 'checksum mismatch' "$TMP/tampered.err"
grep -Fxq 'old phux' "$STABLE_DIR/phux"
if find "$STABLE_DIR" -maxdepth 1 -name '.phux-install*' -print -quit | grep -q .; then
  echo "installer left transaction artifacts after a digest mismatch" >&2
  exit 1
fi

NEXT_SHA=0123456789abcdef0123456789abcdef01234567
NEXT_BINS="$TMP/next-bins"
NEXT_FIXTURE="$TMP/next-fixture"
mkdir -p "$NEXT_BINS" "$NEXT_FIXTURE"
printf 'next phux\n' > "$NEXT_BINS/phux"
printf 'next phux-mcp\n' > "$NEXT_BINS/phux-mcp"
chmod 755 "$NEXT_BINS/phux" "$NEXT_BINS/phux-mcp"
bash "$ROOT/scripts/pack-release.sh" \
  --id "next.${NEXT_SHA}" --target "$TARGET" --bin-dir "$NEXT_BINS" --out-dir "$NEXT_FIXTURE"
write_manifest next "$NEXT_FIXTURE/next.json" "$NEXT_SHA" \
  "$(digest_of "$NEXT_FIXTURE/phux-next.${NEXT_SHA}-${TARGET}.tar.gz")"

NEXT_DIR="$TMP/next-install"
mkdir "$NEXT_DIR"
PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$NEXT_FIXTURE" \
  "$INSTALLER_SH" "$ROOT/scripts/install.sh" --channel next --os linux --arch x86_64 \
    --install-dir "$NEXT_DIR" >"$TMP/next.out" 2>"$TMP/next.err"
grep -Fq "phux: ok: installed phux next.${NEXT_SHA}" "$TMP/next.err"
! grep -Fq "installed phux" "$TMP/next.out"
grep -Fxq next "$NEXT_DIR/.phux-channel"
cmp "$NEXT_BINS/phux" "$NEXT_DIR/phux"
cmp "$NEXT_BINS/phux-mcp" "$NEXT_DIR/phux-mcp"

if PATH="$FAKE_BIN:/usr/bin:/bin" INSTALL_FIXTURE="$NEXT_FIXTURE" \
  "$INSTALLER_SH" "$ROOT/scripts/install.sh" --channel next --version "$VERSION" \
    --os linux --arch x86_64 --install-dir "$NEXT_DIR" \
    >"$TMP/next-conflict.out" 2>"$TMP/next-conflict.err"; then
  echo "installer accepted --channel next with --version" >&2
  exit 1
fi
grep -Fq -- '--version pins a stable tag' "$TMP/next-conflict.err"

output="$(PATH="$FAKE_BIN:/usr/bin:/bin" \
  "$INSTALLER_SH" "$ROOT/scripts/install.sh" --channel latest --version "$VERSION" \
    --os linux --arch x86_64 --install-dir "$NEXT_DIR" --dry-run)"
grep -Fxq 'channel: stable' <<<"$output"

echo "manifest installer tests passed"
