#!/usr/bin/env bash
# Test scripts/gen-formula.sh against the shapes the release matrix can actually
# produce. The fixtures here MUST stay in sync with the build matrix in
# .github/workflows/release.yml: a fixture for a target nothing builds tests a
# code path that can never run, and hides the ones that do. (That is exactly how
# the macOS x86_64 fallback bug survived: the old suite fed gen-formula.sh an
# x86_64-apple-darwin checksum that no release has ever produced.)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/phux-formula-check.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

write_sha() {
  local tag="$1"
  local target="$2"
  local sha="$3"
  printf '%s  phux-%s-%s.tar.gz\n' "$sha" "$tag" "$target" \
    > "${TMP}/phux-${tag}-${target}.tar.gz.sha256"
}

assert_contains() {
  local file="$1"
  local needle="$2"
  if ! grep -Fq -- "$needle" "$file"; then
    echo "error: expected ${file} to contain: ${needle}" >&2
    sed -n '1,160p' "$file" >&2
    exit 1
  fi
}

assert_absent() {
  local file="$1"
  local needle="$2"
  if grep -Fq -- "$needle" "$file"; then
    echo "error: expected ${file} NOT to contain: ${needle}" >&2
    sed -n '1,160p' "$file" >&2
    exit 1
  fi
}

assert_top_level_url() {
  local file="$1"
  local first_url_line
  local first_block_line
  first_url_line="$(grep -n '^  url ' "$file" | head -1 | cut -d: -f1)"
  first_block_line="$(grep -n '^  on_' "$file" | head -1 | cut -d: -f1 || true)"
  if [ -z "$first_url_line" ]; then
    echo "error: generated formula has no top-level url" >&2
    sed -n '1,160p' "$file" >&2
    exit 1
  fi
  if [ -n "$first_block_line" ] && [ "$first_url_line" -gt "$first_block_line" ]; then
    echo "error: generated formula url is only inside a platform block" >&2
    sed -n '1,160p' "$file" >&2
    exit 1
  fi
}

# Every platform the formula does not ship a tarball for must be refused by a
# fatal `depends_on`. A platform with no on_* override silently falls back to the
# top-level url, so "no guard" means "wrong-architecture binary, installed
# successfully, fails at exec time".
assert_no_arch_fallback() {
  local file="$1"
  # A formula with a macOS tarball must pin macOS to arm64 (no x86_64 mac build).
  if grep -Fq 'aarch64-apple-darwin' "$file"; then
    assert_contains "$file" 'depends_on arch: :arm64'
  fi
  assert_absent "$file" 'x86_64-apple-darwin'
}

gen() {
  local tag="$1"
  local out="$2"
  bash "${ROOT}/scripts/gen-formula.sh" "$tag" "$TMP" "$out" >/dev/null
  assert_top_level_url "$out"
  assert_no_arch_fallback "$out"
  if command -v ruby >/dev/null 2>&1; then
    ruby -c "$out" >/dev/null
  fi
}

tag="v9.9.9"
reset_fixtures() { rm -f "${TMP}"/*.sha256; }

arm_mac_sha=1111111111111111111111111111111111111111111111111111111111111111
x86_linux_sha=3333333333333333333333333333333333333333333333333333333333333333
arm_linux_sha=4444444444444444444444444444444444444444444444444444444444444444

# 1. The real, complete release matrix: exactly the three targets release.yml
#    builds. This is the shape that ships; it must be the primary test case.
reset_fixtures
write_sha "$tag" aarch64-apple-darwin "$arm_mac_sha"
write_sha "$tag" x86_64-unknown-linux-gnu "$x86_linux_sha"
write_sha "$tag" aarch64-unknown-linux-gnu "$arm_linux_sha"
gen "$tag" "${TMP}/phux-full.rb"
assert_contains "${TMP}/phux-full.rb" 'url "https://github.com/phall1/phux/releases/download/v9.9.9/phux-v9.9.9-aarch64-apple-darwin.tar.gz"'
assert_contains "${TMP}/phux-full.rb" "sha256 \"${arm_mac_sha}\""
assert_contains "${TMP}/phux-full.rb" 'on_macos do'
assert_contains "${TMP}/phux-full.rb" 'depends_on arch: :arm64'
assert_contains "${TMP}/phux-full.rb" 'on_linux do'
assert_contains "${TMP}/phux-full.rb" 'phux-v9.9.9-x86_64-unknown-linux-gnu.tar.gz'
assert_contains "${TMP}/phux-full.rb" 'phux-v9.9.9-aarch64-unknown-linux-gnu.tar.gz'
# A complete matrix ships both Linux arches, so Linux needs no arch guard.
assert_absent "${TMP}/phux-full.rb" 'depends_on arch: :x86_64'
assert_absent "${TMP}/phux-full.rb" 'depends_on :linux'
assert_absent "${TMP}/phux-full.rb" 'depends_on :macos'

# 2. Partial matrix, macOS only (Linux jobs failed). release.yml is fail-fast:
#    false and publishes whatever built, so this really can reach the tap.
#    Linux must be refused, not served the macOS tarball.
reset_fixtures
write_sha "$tag" aarch64-apple-darwin "$arm_mac_sha"
gen "$tag" "${TMP}/phux-mac-only.rb"
assert_contains "${TMP}/phux-mac-only.rb" 'depends_on :macos'
assert_contains "${TMP}/phux-mac-only.rb" 'depends_on arch: :arm64'
assert_absent "${TMP}/phux-mac-only.rb" 'linux-gnu'

# 3. Partial matrix, Linux only (the macOS job failed). macOS must be refused,
#    not served a Linux tarball.
reset_fixtures
write_sha "$tag" x86_64-unknown-linux-gnu "$x86_linux_sha"
write_sha "$tag" aarch64-unknown-linux-gnu "$arm_linux_sha"
gen "$tag" "${TMP}/phux-linux-only.rb"
assert_contains "${TMP}/phux-linux-only.rb" 'depends_on :linux'
assert_absent "${TMP}/phux-linux-only.rb" 'apple-darwin'

# 4. Partial matrix, single Linux arch. The other Linux arch must be refused
#    rather than falling back to the top-level url.
reset_fixtures
write_sha "$tag" x86_64-unknown-linux-gnu "$x86_linux_sha"
gen "$tag" "${TMP}/phux-linux-x86-only.rb"
assert_contains "${TMP}/phux-linux-x86-only.rb" 'depends_on :linux'
assert_contains "${TMP}/phux-linux-x86-only.rb" 'depends_on arch: :x86_64'
assert_absent "${TMP}/phux-linux-x86-only.rb" 'aarch64-unknown-linux-gnu'

# 5. No artifacts at all is an error, not an empty formula.
reset_fixtures
if bash "${ROOT}/scripts/gen-formula.sh" "$tag" "$TMP" "${TMP}/phux-empty.rb" >/dev/null 2>&1; then
  echo "error: gen-formula.sh accepted an empty dist dir" >&2
  exit 1
fi

# 6. A present but malformed sidecar must fail instead of emitting a broken
# sha256 stanza. Missing sidecars are valid for a partial matrix; malformed ones
# are not.
printf 'not-a-checksum  phux-%s-aarch64-apple-darwin.tar.gz\n' "$tag" \
  > "${TMP}/phux-${tag}-aarch64-apple-darwin.tar.gz.sha256"
if bash "${ROOT}/scripts/gen-formula.sh" "$tag" "$TMP" "${TMP}/phux-invalid.rb" >/dev/null 2>&1; then
  echo "error: gen-formula.sh accepted a malformed checksum sidecar" >&2
  exit 1
fi

echo "formula generation ok"
