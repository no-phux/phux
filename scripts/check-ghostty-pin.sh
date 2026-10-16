#!/usr/bin/env bash
# Verify that the pinned libghostty-rs publishes a prebuilt libghostty-vt for
# every target phux is developed on.
#
# libghostty-vt-sys links a SHA-256-verified archive of its Ghostty commit,
# downloaded from the libghostty-rs release named in its download.rs, and
# falls back to a ~200 s Zig source build when its table has no entry for the
# target. A pin bump onto a libghostty-rs rev whose release was never cut
# would send every developer and CI lane back to Zig without failing anything,
# so this check fails instead. Cargo.lock names the libghostty-rs rev; that
# rev's download.rs names the archives.
#
# FAILURE POLICY: a reachable download.rs without an entry for a target, or a
# listed archive the release does not serve, is a hard failure. An
# unreachable one (offline checkout, GitHub outage, no curl) is a warning and
# exit 0, as in check-zig-pins.sh.
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGETS=(x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu aarch64-apple-darwin x86_64-apple-darwin)

skip() {
  phux_warn "check-ghostty-pin: skipped ($1)"
  exit 0
}

fail() {
  phux_error "check-ghostty-pin: $1"
  exit 1
}

# The source line of the libghostty-vt-sys package in Cargo.lock:
#   source = "git+https://github.com/phall1/libghostty-rs.git?rev=<rev>#<rev>"
source_line="$(awk '
  $0 == "name = \"libghostty-vt-sys\"" { found = 1; next }
  found && /^source = / { print; exit }
  /^\[\[package\]\]/ { found = 0 }
' "$ROOT/Cargo.lock")"
[[ "$source_line" =~ git\+https://github\.com/([^/]+/[^/.]+)(\.git)?\?rev=[0-9a-f]+#([0-9a-f]{40}) ]] ||
  fail "Cargo.lock has no git source for libghostty-vt-sys (got: ${source_line:-nothing})"
fork="${BASH_REMATCH[1]}"
fork_rev="${BASH_REMATCH[3]}"

command -v curl >/dev/null 2>&1 || skip "curl not available"
raw="https://raw.githubusercontent.com/${fork}/${fork_rev}/crates/libghostty-vt-sys"
build_rs="$(curl -fsSL --max-time 30 "$raw/build.rs" 2>/dev/null)" || skip "could not fetch $raw/build.rs"
download_rs="$(curl -fsSL --max-time 30 "$raw/download.rs" 2>/dev/null)" ||
  fail "${fork}@${fork_rev:0:12} has no crates/libghostty-vt-sys/download.rs (no prebuilt archives)"

const() { sed -n "s|^const $1: &str = \"\\(.*\\)\";\$|\\1|p"; }
commit="$(const GHOSTTY_COMMIT <<<"$build_rs")"
base="$(const PREBUILT_BASE_URL <<<"$download_rs")"
optimize="$(const PREBUILT_OPTIMIZE <<<"$download_rs")"
cpu="$(const PREBUILT_CPU <<<"$download_rs")"
[ -n "$commit" ] && [ -n "$base" ] && [ -n "$optimize" ] && [ -n "$cpu" ] ||
  fail "could not read the Ghostty commit and prebuilt flavor from ${fork}@${fork_rev:0:12}"
tag="ghostty-vt-${commit:0:12}-$(tr '[:upper:]' '[:lower:]' <<<"$optimize")-${cpu}"

# rustfmt may split a table entry over lines; match it on one.
table="$(tr -s '[:space:]' ' ' <<<"$download_rs")"
for target in "${TARGETS[@]}"; do
  grep -Eq "\( ?\"$target\", ?\"[0-9a-f]{64}\",? ?\)" <<<"$table" || {
    phux_error "check-ghostty-pin: ${fork}@${fork_rev:0:12} publishes no prebuilt libghostty-vt for $target"
    phux_hint "Run the prebuilt workflow in $fork and land its PREBUILT_ARCHIVES table before bumping the pin."
    exit 1
  }
  url="$base/$tag/libghostty-vt-$target.tar.gz"
  curl -fsSIL --max-time 30 -o /dev/null "$url" 2>/dev/null || fail "$url is not served"
done

phux_ok "check-ghostty-pin: ${fork}@${fork_rev:0:12} publishes $tag for ${#TARGETS[@]} targets"
