#!/usr/bin/env bash
# Verify that flake.nix builds libghostty-vt from the Ghostty commit the pinned
# libghostty-rs builds it from.
#
# The devshell links the flake's prebuilt libghostty-vt (LIBGHOSTTY_VT_SYS_LIB_DIR)
# in place of the archive libghostty-vt-sys's build script would make from its
# own GHOSTTY_COMMIT. The Rust bindings are generated from that commit's
# headers, so a flake `ghostty` input at any other commit links a C ABI the
# bindings do not describe. Nothing else ties the two pins together: Cargo.lock
# names the libghostty-rs rev, and that rev's build.rs names the commit.
#
# FAILURE POLICY: a reachable build.rs whose commit differs is a hard failure.
# An unreachable one (offline checkout, GitHub outage, no curl) is a warning
# and exit 0, as in check-zig-pins.sh.
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

skip() {
  phux_warn "check-ghostty-pin: skipped ($1)"
  exit 0
}

fail() {
  phux_error "check-ghostty-pin: $1"
  exit 1
}

command -v jq >/dev/null 2>&1 || skip "jq not available"

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

flake_owner="$(jq -r '.nodes.ghostty.locked.owner // ""' "$ROOT/flake.lock")"
flake_repo="$(jq -r '.nodes.ghostty.locked.repo // ""' "$ROOT/flake.lock")"
flake_rev="$(jq -r '.nodes.ghostty.locked.rev // ""' "$ROOT/flake.lock")"
[ -n "$flake_rev" ] || fail "flake.lock has no locked ghostty input"

command -v curl >/dev/null 2>&1 || skip "curl not available"
url="https://raw.githubusercontent.com/${fork}/${fork_rev}/crates/libghostty-vt-sys/build.rs"
build_rs="$(curl -fsSL --max-time 30 "$url" 2>/dev/null)" || skip "could not fetch $url"

pinned_commit="$(sed -n 's/^const GHOSTTY_COMMIT: &str = "\([0-9a-f]\{40\}\)";$/\1/p' <<<"$build_rs")"
pinned_repo="$(sed -n 's|^const GHOSTTY_REPO: &str = "https://github.com/\([^"]*\)\.git";$|\1|p' <<<"$build_rs")"
[ -n "$pinned_commit" ] || fail "no GHOSTTY_COMMIT in $url"

if [ "$pinned_commit" != "$flake_rev" ] || [ "$pinned_repo" != "$flake_owner/$flake_repo" ]; then
  phux_error "check-ghostty-pin: flake.lock builds libghostty-vt from $flake_owner/$flake_repo@$flake_rev,"
  phux_info "but ${fork}@${fork_rev:0:12} (Cargo.lock) pins $pinned_repo@$pinned_commit."
  phux_hint "Point the ghostty input in flake.nix at github:$pinned_repo/$pinned_commit and run nix flake lock."
  exit 1
fi

phux_ok "check-ghostty-pin: ghostty ${flake_rev:0:12} matches ${fork}@${fork_rev:0:12}"
