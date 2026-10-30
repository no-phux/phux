#!/usr/bin/env bash
# scripts/gen-formula.sh renders a formula whose macOS install drops
# quarantine without failing when the attribute is already absent.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

digest() { printf '%s\n' "$1"; }
write_sha() {
  printf '%s  phux-%s-%s.tar.gz\n' "$1" "$2" "$3" > "$TMP/dist/phux-$2-$3.tar.gz.sha256"
}

mkdir -p "$TMP/dist"
arm_mac="$(printf 'a%.0s' {1..64})"
x86_linux="$(printf 'b%.0s' {1..64})"
arm_linux="$(printf 'c%.0s' {1..64})"
write_sha "$arm_mac" v0.1.0 aarch64-apple-darwin
write_sha "$x86_linux" v0.1.0 x86_64-unknown-linux-gnu
write_sha "$arm_linux" v0.1.0 aarch64-unknown-linux-gnu

bash "$ROOT/scripts/gen-formula.sh" v0.1.0 "$TMP/dist" "$TMP/phux.rb" >/dev/null
formula="$(cat "$TMP/phux.rb")"
grep -Fq 'url "https://github.com/no-phux/phux/releases/download/v0.1.0/phux-v0.1.0-aarch64-apple-darwin.tar.gz"' <<<"$formula" || fail "primary url"
grep -Fq "sha256 \"${arm_mac}\"" <<<"$formula" || fail "primary digest"
grep -Fq "sha256 \"${x86_linux}\"" <<<"$formula" || fail "linux digest"
grep -Fq "sha256 \"${arm_linux}\"" <<<"$formula" || fail "arm linux digest"
grep -Fq 'quiet_system "xattr", "-d", "com.apple.quarantine", installed' <<<"$formula" || fail "quarantine clear"
grep -Fq 'return unless OS.mac?' <<<"$formula" || fail "macos guard"
if grep -Eq 'brew trust|postflight' <<<"$formula"; then fail "formula must not add a trust step"; fi
if command -v ruby >/dev/null 2>&1; then
  ruby -c "$TMP/phux.rb" >/dev/null || fail "ruby syntax"
fi

# A checksum that is not 64 lowercase hex digits is refused.
printf 'GGGG  phux-v0.1.0-aarch64-apple-darwin.tar.gz\n' > "$TMP/dist/phux-v0.1.0-aarch64-apple-darwin.tar.gz.sha256"
if bash "$ROOT/scripts/gen-formula.sh" v0.1.0 "$TMP/dist" "$TMP/bad.rb" >/dev/null 2>"$TMP/err"; then
  fail "accepted an invalid checksum"
fi
grep -Fq 'invalid checksum' "$TMP/err" || fail "invalid checksum message: $(cat "$TMP/err")"

rm -rf "$TMP/dist"
mkdir -p "$TMP/dist"
if bash "$ROOT/scripts/gen-formula.sh" v0.1.0 "$TMP/dist" "$TMP/empty.rb" >/dev/null 2>"$TMP/err"; then
  fail "accepted an empty dist"
fi
grep -Fq 'no artifacts found' "$TMP/err" || fail "empty dist message: $(cat "$TMP/err")"

echo "formula renderer tests passed"
