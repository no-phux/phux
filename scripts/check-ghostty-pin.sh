#!/usr/bin/env bash
# Verify immutable engine source and report actual prebuilt coverage.
#
# libghostty-vt-sys links a SHA-256-verified archive of its Ghostty commit,
# downloaded from the libghostty-rs release named in its download.rs, and
# falls back to a Zig source build when its table has no entry for the target.
# Source-only pins are supported: this gate must never require cutting a release
# or accept old archive hashes as evidence for a new engine. Cargo.lock names
# the immutable libghostty-rs rev; its build.rs pins the engine source and its
# download.rs names any separately published archives.
#
# FAILURE POLICY: malformed pins, missing engine source, or a listed archive the
# release does not serve are hard failures. An unreachable service (offline,
# GitHub outage, no curl) is a warning and exit 0, never a verification claim.
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
[[ "$source_line" =~ git\+https://github\.com/([^/]+/[^/.]+)(\.git)?\?rev=[0-9a-f]{40}#([0-9a-f]{40}) ]] ||
  fail "Cargo.lock has no git source for libghostty-vt-sys (got: ${source_line:-nothing})"
fork="${BASH_REMATCH[1]}"
fork_rev="${BASH_REMATCH[3]}"
[[ "$source_line" == *"?rev=$fork_rev#$fork_rev"* ]] ||
  fail "Cargo.lock binding revision does not match its resolved commit"

command -v curl >/dev/null 2>&1 || skip "curl not available"

# Keep HTTP absence distinct from network failure; a typo is not an outage.
fetch_source() {
  local url="$1" response status
  response="$(curl -sSL --max-time 30 -w $'\n%{http_code}' "$url" 2>/dev/null)" ||
    skip "could not fetch $url"
  status="${response##*$'\n'}"
  case "$status" in
    2??) fetched="${response%$'\n'*}" ;;
    4??) fail "$url is not served (HTTP $status)" ;;
    *) skip "$url is unreachable (HTTP $status)" ;;
  esac
}

raw="https://raw.githubusercontent.com/${fork}/${fork_rev}/crates/libghostty-vt-sys"
fetch_source "$raw/build.rs"
build_rs="$fetched"
fetch_source "$raw/download.rs"
download_rs="$fetched"

const() { sed -n "s|^const $1: &str = \"\\(.*\\)\";\$|\\1|p"; }
commit="$(const GHOSTTY_COMMIT <<<"$build_rs")"
engine_repo="$(const GHOSTTY_REPO <<<"$build_rs")"
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || fail "engine revision is not an immutable full commit: $commit"
[[ "$engine_repo" =~ ^https://github\.com/([^/]+/[^/.]+)(\.git)?$ ]] ||
  fail "cannot verify engine source repository: $engine_repo"
engine_fork="${BASH_REMATCH[1]}"
browser_build="$ROOT/scripts/build-vt-wasm.sh"
browser_commit="$(sed -n 's/^revision=//p' "$browser_build")"
[[ "$browser_commit" == "$commit" ]] ||
  fail "native engine $commit differs from browser engine $browser_commit"
browser_sha="$(sed -n 's/^archive_sha256=//p' "$browser_build")"
[[ "$browser_sha" =~ ^[0-9a-f]{64}$ ]] || fail "browser engine source checksum is missing or malformed"
grep -Fq "https://codeload.github.com/$engine_fork/tar.gz/\$revision" "$browser_build" ||
  fail "native and browser engine source repositories differ"
fetch_source "https://raw.githubusercontent.com/$engine_fork/$commit/build.zig.zon"
engine_zig="$(sed -n 's/.*\.minimum_zig_version = "\([^"]*\)".*/\1/p' <<<"$fetched")"
expected_zig="$(sed -n 's/^[[:space:]]*"version": "\([^"]*\)",*/\1/p' "$ROOT/.config/zig-toolchain.json")"
[[ -n "$engine_zig" && "$engine_zig" == "$expected_zig" ]] ||
  fail "engine requires Zig ${engine_zig:-unknown}; toolchain pins $expected_zig"
phux_ok "check-ghostty-pin: immutable engine source $engine_fork@$commit verified"
base="$(const PREBUILT_BASE_URL <<<"$download_rs")"
optimize="$(const PREBUILT_OPTIMIZE <<<"$download_rs")"
cpu="$(const PREBUILT_CPU <<<"$download_rs")"
[ -n "$commit" ] && [ -n "$base" ] && [ -n "$optimize" ] && [ -n "$cpu" ] ||
  fail "could not read the Ghostty commit and prebuilt flavor from ${fork}@${fork_rev:0:12}"
tag="ghostty-vt-${commit:0:12}-$(tr '[:upper:]' '[:lower:]' <<<"$optimize")-${cpu}"

# Whether the release at $base/$tag lists ARCHIVE, per the GitHub API; an
# unreachable API is an outage, so the check skips.
release_json=""
listed() {
  if [ -z "$release_json" ]; then
    [[ "$base" =~ ^https://github\.com/([^/]+/[^/]+)/releases/download$ ]] ||
      skip "$base is not a GitHub release download base"
    local api="https://api.github.com/repos/${BASH_REMATCH[1]}/releases/tags/$tag"
    local auth=()
    [ -z "${GH_TOKEN:-}" ] || auth=(-H "Authorization: Bearer $GH_TOKEN")
    release_json="$(curl -fsSL --max-time 30 "${auth[@]}" "$api" 2>/dev/null)" ||
      skip "GitHub did not serve $base/$tag and its API is unreachable"
  fi
  tr ',' '\n' <<<"$release_json" | grep -Eq "\"name\": ?\"$1\""
}

# rustfmt may split a table entry over lines; match it on one.
table="$(awk '/^const PREBUILT_ARCHIVES:/ { found=1 } found { print; if (/;/) exit }' <<<"$download_rs" | tr -s '[:space:]' ' ')"
grep -Fq 'const PREBUILT_ARCHIVES:' <<<"$table" || fail "prebuilt coverage table is missing"
prebuilt=0
source_only=0
for target in "${TARGETS[@]}"; do
  if ! grep -Fq "\"$target\"" <<<"$table"; then
    phux_info "check-ghostty-pin: $target uses verified source (no prebuilt declared)"
    source_only=$((source_only + 1))
    continue
  fi
  grep -Eq "\( ?\"$target\", ?\"[0-9a-f]{64}\",? ?\)" <<<"$table" ||
    fail "malformed prebuilt checksum for $target"
  prebuilt=$((prebuilt + 1))
  archive="libghostty-vt-$target.tar.gz"
  url="$base/$tag/$archive"
  status="$(curl -sIL --max-time 30 -o /dev/null -w '%{http_code}' "$url" 2>/dev/null)" || status=000
  case "$status" in
    2??) continue ;;
    4??) fail "$url is not served" ;;
  esac
  # releases/download can answer one asset with a 5xx for hours; the API
  # still says whether the release has it.
  listed "$archive" || fail "$url is not served (HTTP $status, and the release does not list it)"
done

phux_ok "check-ghostty-pin: ${fork}@${fork_rev:0:12}: $prebuilt prebuilt, $source_only source-only targets ($tag)"
