#!/usr/bin/env bash
# scripts/install.sh --rune, through dash where the host has it: reading the
# update feed, the Linux install transaction, the menu entry, and what must be
# refused. The network is a fake curl. A macOS install needs a real signed
# bundle, so this covers that path only as a dry run.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
SH="$(command -v dash || echo /bin/sh)"
BASE=https://github.com/no-phux/phux/releases/download
VERSION=0.1.0-alpha.9
ARCHIVE="phux-rune-$VERSION-linux-x86_64.tar.gz"
STEM="${ARCHIVE%.tar.gz}"
export HOME="$TMP/home" SHELL=/bin/zsh XDG_DATA_HOME="$TMP/data"
unset PHUX_CHANNEL PHUX_INSTALL_DIR PHUX_DESKTOP_APPLICATIONS_DIR PHUX_REQUIRE_SIGNATURE
mkdir -p "$HOME/.local/bin" "$TMP/bin" "$TMP/served" "$TMP/pack"
export SERVED="$TMP/served"

cat > "$TMP/bin/curl" <<'EOF'
#!/bin/sh
out="" url=""
while [ "$#" -gt 0 ]; do
  case "$1" in -o | -O) out="$2"; shift ;; https://*) url="$1" ;; esac
  shift
done
[ -f "$SERVED/${url##*/}" ] || exit 22
cp "$SERVED/${url##*/}" "$out"
EOF
chmod 755 "$TMP/bin/curl"

fail() { echo "FAIL: $*" >&2; exit 1; }
installer() {
  PATH="${TEST_PATH:-$TMP/bin:/usr/bin:/bin}" "$SH" "${TEST_SCRIPT:-$ROOT/scripts/install.sh}" \
    --os linux --arch x86_64 --rune "$@"
}
refused() {
  local message="$1"
  shift
  if installer "$@" >"$TMP/out" 2>"$TMP/err"; then fail "accepted: $*"; fi
  grep -Fq -- "$message" "$TMP/err" || fail "expected '$message', got: $(cat "$TMP/err")"
}
sha() { (shasum -a 256 "$1" 2>/dev/null || sha256sum "$1") | cut -d' ' -f1; }

# A release of MARKER: the binary reports REPORTED and carries the marker.
pack() {
  local marker="$1" reported="${2:-$VERSION}" dir="$TMP/pack/$STEM"
  rm -rf "$dir"
  mkdir -p "$dir"
  printf '#!/bin/sh\n# %s\necho "phux-rune %s"\n' "$marker" "$reported" > "$dir/phux-rune"
  chmod 755 "$dir/phux-rune"
  printf '%s\n' "$VERSION" > "$dir/phux-rune.release"
  printf 'license\n' > "$dir/LICENSE"
  printf 'notice\n' > "$dir/NOTICE"
  cp "$ROOT/clients/rune/packaging/sh.phux.rune.desktop" "$dir/"
  printf 'png' > "$dir/sh.phux.rune.png"
}
serve() {
  tar -C "$TMP/pack" -czf "$SERVED/$ARCHIVE" "$STEM"
  feed "$(sha "$SERVED/$ARCHIVE")" "${1:-$VERSION}"
}
feed() {
  cat > "$SERVED/rune.json" <<EOF
{
  "schema": 1,
  "version": "$2",
  "sha": "$(printf 'a%.0s' {1..40})",
  "protocol": "3.4",
  "targets": {
    "aarch64-apple-darwin": {
      "sha256": "$(printf 'b%.0s' {1..64})",
      "size": 7
    },
    "x86_64-unknown-linux-gnu": {
      "sha256": "$1",
      "size": 7
    }
  }
}
EOF
}

# The app finds this CLI, so the installer leaves it alone.
printf '#!/bin/sh\n' > "$HOME/.local/bin/phux"
chmod 755 "$HOME/.local/bin/phux"

# --- a plan -------------------------------------------------------------------
pack first
serve
out="$(PATH="$TMP/bin:/usr/bin:/bin" "$SH" "$ROOT/scripts/install.sh" --dry-run --os darwin --arch arm64 --rune)"
grep -Fxq "url: $BASE/rune-v$VERSION/phux-rune-$VERSION-macos-arm64.zip" <<<"$out" || fail "macOS url: $out"
grep -Fxq "sha256: $(printf 'b%.0s' {1..64})" <<<"$out" || fail "macOS digest: $out"
grep -Fxq 'product: rune' <<<"$out" || fail "macOS product: $out"
out="$(installer --dry-run)"
grep -Fxq "url: $BASE/rune-v$VERSION/$ARCHIVE" <<<"$out" || fail "Linux url: $out"

# --- an install ---------------------------------------------------------------
installer >"$TMP/out" 2>"$TMP/err" || fail "install: $(cat "$TMP/err")"
bundle="$XDG_DATA_HOME/phux-rune"
grep -q first "$bundle/phux-rune" || fail "binary not installed"
for file in phux-rune.release LICENSE NOTICE; do
  [[ -f "$bundle/$file" ]] || fail "$file not installed"
done
grep -Fq 'without its release signature' "$TMP/err" || fail "an unsigned install must say so"
[[ -L "$HOME/.local/bin/phux-rune" ]] || fail "no launcher"
[[ "$("$HOME/.local/bin/phux-rune")" == "phux-rune $VERSION" ]] || fail "launcher does not run"
entry="$XDG_DATA_HOME/applications/sh.phux.rune.desktop"
real="$(cd "$bundle" && pwd -P)"
grep -Fxq "Exec=\"$real/phux-rune\"" "$entry" || fail "Exec: $(cat "$entry")"
grep -Fxq "Icon=$real/sh.phux.rune.png" "$entry" || fail "Icon: $(cat "$entry")"

# An update replaces everything; nothing is left beside it.
pack second
serve
installer >/dev/null 2>"$TMP/err" || fail "update: $(cat "$TMP/err")"
grep -q second "$bundle/phux-rune" || fail "not updated"
[[ -z "$(find "$XDG_DATA_HOME" -name '.phux-desktop-install*')" ]] || fail "install litter"

# --- refusals, with the install untouched ------------------------------------
pack third
serve
feed "$(printf 'c%.0s' {1..64})" "$VERSION"
refused 'checksum mismatch'
pack third 0.0.1
serve
refused 'not version'
pack third
printf 'x' > "$TMP/pack/$STEM/extra"
serve
refused 'unexpected archive member'
pack third
ln -s /etc/passwd "$TMP/pack/$STEM/NOTICE.link"
serve
refused 'not links'
pack third
serve 0.1.0-beta.1
refused 'invalid Phux Rune version'

# --- signatures ---------------------------------------------------------------

# With minisign on PATH the signed sidecar outranks the unsigned feed. A fake
# drives the installer's side; the real one proves the invocation below.
pack signed
serve
digest="$(sha "$SERVED/$ARCHIVE")"
printf '%s  %s\n' "$digest" "$ARCHIVE" > "$SERVED/$ARCHIVE.sha256"
printf 'signature\n' > "$SERVED/$ARCHIVE.sha256.minisig"
mkdir -p "$TMP/sig-bin"
export CALLS="$TMP/minisign-calls"
cat > "$TMP/sig-bin/minisign" <<'EOF'
#!/bin/sh
printf 'minisign %s\n' "$*" >> "$CALLS"
exit "${MINISIGN_STATUS:-0}"
EOF
chmod 755 "$TMP/sig-bin/minisign"
SIG_PATH="$TMP/bin:$TMP/sig-bin:/usr/bin:/bin"
MINISIGN_STATUS=1 TEST_PATH="$SIG_PATH" refused 'is not signed by a phux release key'
feed "$(printf 'c%.0s' {1..64})" "$VERSION"
TEST_PATH="$SIG_PATH" refused "the signed checksum for $ARCHIVE is $digest"
feed "$digest" "$VERSION"
rm "$SERVED/$ARCHIVE.sha256.minisig"
TEST_PATH="$SIG_PATH" refused 'an unsigned release is never installed'
PHUX_REQUIRE_SIGNATURE=1 refused 'PHUX_REQUIRE_SIGNATURE=1 but minisign is not on PATH'
grep -q second "$bundle/phux-rune" || fail "a refused update changed the install"

printf 'signature\n' > "$SERVED/$ARCHIVE.sha256.minisig"
TEST_PATH="$SIG_PATH" installer >/dev/null 2>"$TMP/err" || fail "signed install: $(cat "$TMP/err")"
grep -q signed "$bundle/phux-rune" || fail "not installed from the signed release"
grep -Fq 'minisign -V -H -q -P RWTJVmT0UFkQn2Zw' "$CALLS" || fail "the signature must be checked, prehashed, against the release key"

# CI sets PHUX_TEST_REAL_MINISIGN=1 and provides minisign, so this never
# silently skips there.
real="$(command -v minisign || true)"
if [[ -z $real && ${PHUX_TEST_REAL_MINISIGN:-0} == 1 ]]; then
  fail "PHUX_TEST_REAL_MINISIGN=1 but minisign is not on PATH"
fi
if [[ -n $real ]]; then
  mkdir -p "$TMP/real-sig-bin" "$TMP/keys"
  ln -s "$real" "$TMP/real-sig-bin/minisign"
  REAL_PATH="$TMP/bin:$TMP/real-sig-bin:/usr/bin:/bin"
  minisign -G -W -p "$TMP/keys/test.pub" -s "$TMP/keys/test.key" >/dev/null
  minisign -S -s "$TMP/keys/test.key" -m "$SERVED/$ARCHIVE.sha256" \
    -x "$SERVED/$ARCHIVE.sha256.minisig" </dev/null >/dev/null
  # A host that rewrites the assets can sign, but not with a release key.
  TEST_PATH="$REAL_PATH" refused 'is not signed by a phux release key'
  # The same installer trusting that key accepts it, and refuses a tampered sidecar.
  key="$(sed -n 2p "$TMP/keys/test.pub")"
  sed "s|^RELEASE_KEYS=.*|RELEASE_KEYS='$key'|" "$ROOT/scripts/install.sh" > "$TMP/install-testkey.sh"
  TEST_SCRIPT="$TMP/install-testkey.sh" TEST_PATH="$REAL_PATH" installer >/dev/null 2>"$TMP/err" \
    || fail "a real signature: $(cat "$TMP/err")"
  printf '%s  %s\n' "$(printf 'd%.0s' {1..64})" "$ARCHIVE" > "$SERVED/$ARCHIVE.sha256"
  TEST_SCRIPT="$TMP/install-testkey.sh" TEST_PATH="$REAL_PATH" refused 'is not signed by a phux release key'
fi

mkdir -p "$TMP/other/phux-rune"
pack third
serve
refused 'unrelated' --applications-dir "$TMP/other"
refused 'combine with --rune' --desktop
refused 'combine with --rune' --channel next
refused 'combine with --rune' --version 1.2.3
if PATH="$TMP/bin:/usr/bin:/bin" "$SH" "$ROOT/scripts/install.sh" --os linux --arch arm64 --rune >/dev/null 2>"$TMP/err"; then
  fail "installed for arm64 Linux"
fi
grep -Fq 'no Phux Rune build' "$TMP/err" || fail "arm64: $(cat "$TMP/err")"

# With no CLI anywhere the plan installs one first.
rm "$HOME/.local/bin/phux"
if ! command -v phux >/dev/null 2>&1 || [[ "$(command -v phux)" != /usr/bin/* ]]; then
  cp "$ROOT/scripts/fixtures/release-manifest/stable.json" "$SERVED/stable.json"
  out="$(PATH="$TMP/bin:/usr/bin:/bin" "$SH" "$ROOT/scripts/install.sh" --dry-run --os linux --arch x86_64 --rune 2>/dev/null)"
  grep -Fxq 'product: phux' <<<"$out" || fail "no CLI plan: $out"
  grep -Fxq 'product: rune' <<<"$out" || fail "no Rune plan: $out"
fi
echo "install --rune checks passed"
