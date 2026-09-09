#!/usr/bin/env bash
# Verify the Zig tarball checksums pinned in .config/zig-toolchain.json
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
MANIFEST="$ROOT/.config/zig-toolchain.json"
INDEX_URL="https://ziglang.org/download/index.json"

skip() {
  printf 'check-zig-pins: SKIPPED (%s)\n' "$1" >&2
  exit 0
}

test -f "$MANIFEST" || {
  printf 'check-zig-pins: missing %s\n' "$MANIFEST" >&2
  exit 1
}

command -v jq >/dev/null 2>&1 || skip "jq not available"
jq -e '
  .version as $v |
  ($v | test("^[0-9]+\\.[0-9]+\\.[0-9]+$")) and
  (.archives | keys == ["aarch64-linux", "aarch64-macos", "x86_64-linux"]) and
  all(.archives | to_entries[];
    .value.archive == ("zig-" + .key + "-" + $v + ".tar.xz") and
    (.value.sha256 | test("^[0-9a-f]{64}$")))
' "$MANIFEST" >/dev/null
source "$ROOT/scripts/lib/dev-toolchain.sh"
version="$ZIG_VERSION"
pins="$(jq -r '.archives | to_entries[] | [.key, .value.sha256] | @tsv' "$MANIFEST")"
# The bootstrap reader uses only standard shell tools. Check it against the
# parsed manifest so formatting changes cannot silently break native setup.
while read -r target sha; do
  test "$(zig_digest "$target")" = "$sha"
done <<<"$pins"
test "$version" = "$(jq -r .version "$MANIFEST")"
command -v curl >/dev/null 2>&1 || skip "curl not available"

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
    printf 'check-zig-pins: %s: no %s tarball in the index for Zig %s\n' "${MANIFEST#"$ROOT/"}" "$target" "$version" >&2
    failures=$((failures + 1))
  elif [ "$upstream" != "$sha" ]; then
    printf 'check-zig-pins: %s: %s digest is stale for Zig %s\n  pinned:   %s\n  upstream: %s\n' \
      "${MANIFEST#"$ROOT/"}" "$target" "$version" "$sha" "$upstream" >&2
    failures=$((failures + 1))
  fi
done <<<"$pins"

if [ "$failures" -ne 0 ]; then
  printf 'check-zig-pins: %d stale Zig pin(s). Re-pin from %s before releasing.\n' "$failures" "$INDEX_URL" >&2
  exit 1
fi

printf 'check-zig-pins: OK (Zig %s, %d target(s))\n' "$version" "$(echo "$pins" | grep -c .)"
