#!/usr/bin/env bash
# Verify the Zig tarball checksums pinned in .github/workflows/release.yml
# against the digests ziglang.org publishes for the pinned ZIG_VERSION.
#
# WHY THIS EXISTS: v0.10.0 released with zero assets. ZIG_VERSION had been
# bumped to 0.16.0 while all three `sha=` literals still held 0.15.2's digests,
# so every matrix leg died at `shasum -a 256 -c -` before installing a compiler.
# Nothing in the repo tied the version to the digests, and release.yml runs only
# after release-please has already pushed the tag and created the release — so
# the first signal was a published release that could never gain artifacts.
#
# The pins stay hand-written on purpose: a checksum fetched at build time
# verifies nothing against the server that served the tarball. That makes an
# out-of-band comparison the only thing that can catch a stale digest, which is
# what this script is.
#
# FAILURE POLICY: a digest that is reachable and WRONG is a hard failure. An
# unreachable index (offline checkout, ziglang.org outage, no curl) is a warning
# and exit 0 — this guards a release path, and it must not redden PRs that have
# nothing to do with Zig.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW="$ROOT/.github/workflows/release.yml"
INDEX_URL="https://ziglang.org/download/index.json"

skip() {
  printf 'check-zig-pins: SKIPPED (%s)\n' "$1" >&2
  exit 0
}

test -f "$WORKFLOW" || {
  printf 'check-zig-pins: missing %s\n' "$WORKFLOW" >&2
  exit 1
}

version="$(sed -n 's/^[[:space:]]*ZIG_VERSION:[[:space:]]*\([0-9][^[:space:]]*\).*/\1/p' "$WORKFLOW" | head -n 1)"
if [ -z "$version" ]; then
  printf 'check-zig-pins: no ZIG_VERSION pin found in %s\n' "${WORKFLOW#"$ROOT/"}" >&2
  exit 1
fi

# Pair each `archive="zig-<zig-target>-${ZIG_VERSION}.tar.xz"` with the `sha="..."`
# that follows it. The zig-target (e.g. aarch64-macos) is the key into the index,
# and is NOT the Rust triple the case arm matches on. Parsed in bash rather than
# awk because gawk's 3-argument match() is absent from the BSD awk on macOS,
# where `just shellcheck` and release-preflight also run.
pins=""
pending=""
while IFS= read -r line; do
  case "$line" in
    *'archive="zig-'*)
      pending="${line#*archive=\"zig-}"
      pending="${pending%%-\$\{ZIG_VERSION\}*}"
      ;;
    *'sha="'*)
      if [ -n "$pending" ]; then
        sha="${line#*sha=\"}"
        sha="${sha%%\"*}"
        pins="${pins}${pending} ${sha}"$'\n'
        pending=""
      fi
      ;;
  esac
done <"$WORKFLOW"
pins="${pins%$'\n'}"

if [ -z "$pins" ]; then
  printf 'check-zig-pins: found no archive/sha pairs in %s\n' "${WORKFLOW#"$ROOT/"}" >&2
  exit 1
fi

command -v curl >/dev/null 2>&1 || skip "curl not available"
command -v jq >/dev/null 2>&1 || skip "jq not available"

index="$(curl -fsSL --max-time 30 "$INDEX_URL" 2>/dev/null)" || skip "could not fetch $INDEX_URL"
echo "$index" | jq -e . >/dev/null 2>&1 || skip "$INDEX_URL did not return JSON"

if ! echo "$index" | jq -e --arg v "$version" 'has($v)' >/dev/null; then
  # Zig prunes old versions from the index once they stop being the latest
  # release. That is a stale pin worth knowing about, but it is not evidence
  # that the digests are wrong, so it warns rather than fails.
  printf 'check-zig-pins: WARNING: %s is not in the download index (pruned or mistyped); digests unverified\n' "$version" >&2
  exit 0
fi

failures=0
while read -r target sha; do
  [ -n "$target" ] || continue
  upstream="$(echo "$index" | jq -r --arg v "$version" --arg t "$target" '.[$v][$t].shasum // ""')"
  if [ -z "$upstream" ]; then
    printf 'check-zig-pins: %s: no %s tarball in the index for Zig %s\n' "${WORKFLOW#"$ROOT/"}" "$target" "$version" >&2
    failures=$((failures + 1))
  elif [ "$upstream" != "$sha" ]; then
    printf 'check-zig-pins: %s: %s digest is stale for Zig %s\n  pinned:   %s\n  upstream: %s\n' \
      "${WORKFLOW#"$ROOT/"}" "$target" "$version" "$sha" "$upstream" >&2
    failures=$((failures + 1))
  fi
done <<<"$pins"

if [ "$failures" -ne 0 ]; then
  printf 'check-zig-pins: %d stale Zig pin(s). Re-pin from %s before releasing.\n' "$failures" "$INDEX_URL" >&2
  exit 1
fi

printf 'check-zig-pins: OK (Zig %s, %d target(s))\n' "$version" "$(echo "$pins" | grep -c .)"
