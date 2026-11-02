#!/usr/bin/env bash
# Prove a packaged archive starts, the way a person gets it: unpacked, against
# a same-source server on a temp socket, with nothing of this machine's phux in
# reach. Clients refuse older versions, so this is the last place a build that
# dies at launch can be stopped.
#
#   clients/rune/scripts/qualify.sh ARCHIVE PHUX [EVIDENCE_DIR]
#
# PHUX is a dev build (target/debug/phux). Checks the notices, what the binary
# links, its version, and that a headless frame shows the session; on Linux it
# also opens a real window under Xvfb.
set -euo pipefail

archive="${1:?usage: qualify.sh ARCHIVE PHUX [EVIDENCE_DIR]}"
phux="${2:?usage: qualify.sh ARCHIVE PHUX [EVIDENCE_DIR]}"
evidence="${3:-}"
archive="$(cd "$(dirname "$archive")" && pwd)/$(basename "$archive")"
phux="$(cd "$(dirname "$phux")" && pwd)/$(basename "$phux")"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
marker=rune-qualification
# What the ubuntu-24.04 build host links; see docs/releasing.md.
glibc_floor=2.39

# Under /tmp: a Unix socket path has to stay short.
scratch="$(mktemp -d /tmp/rune-qualify.XXXXXX)"
server=""
cleanup() {
    if [[ -n "$server" ]]; then
        kill "$server" 2>/dev/null || true
        wait "$server" 2>/dev/null || true
    fi
    if [[ -n "$evidence" ]]; then
        mkdir -p "$evidence"
        cp "$scratch"/*.png "$scratch"/*.json "$scratch"/*.log "$evidence"/ 2>/dev/null || true
    fi
    rm -rf "$scratch"
}
trap cleanup EXIT

fail() {
    echo "qualify: $*" >&2
    exit 1
}

# No inherited PHUX_* can aim this at a real server. Only what a GPU needs on a
# hosted runner comes through.
clean=(env -i HOME="$scratch/home" PATH=/usr/bin:/bin SHELL=/bin/sh TERM=dumb TMPDIR="$scratch"
    PHUX_PROFILE=rune-qualify RUNE_PREFS_DIR="$scratch/prefs"
    ${VK_DRIVER_FILES:+VK_DRIVER_FILES="$VK_DRIVER_FILES"}
    ${WGPU_BACKEND:+WGPU_BACKEND="$WGPU_BACKEND"})

mkdir -p "$scratch/home" "$scratch/prefs" "$scratch/unpacked"
case "$archive" in
*.zip)
    ditto -x -k "$archive" "$scratch/unpacked"
    app="$scratch/unpacked/Phux Rune.app"
    binary="$app/Contents/MacOS/phux-rune"
    notices="$app/Contents/Resources"
    floor="$(plutil -extract LSMinimumSystemVersion raw "$app/Contents/Info.plist")"
    PHUX_MACOS_MIN_MAX="$floor" bash "$repo/scripts/check-binary-portability.sh" "$binary"
    ;;
*.tar.gz)
    tar -xzf "$archive" -C "$scratch/unpacked"
    notices="$(find "$scratch/unpacked" -mindepth 1 -maxdepth 1 -type d)"
    binary="$notices/phux-rune"
    PHUX_GLIBC_MAX="$glibc_floor" bash "$repo/scripts/check-binary-portability.sh" "$binary"
    ;;
*) fail "not a release archive: $archive" ;;
esac

for name in LICENSE NOTICE THIRD-PARTY-NOTICES.txt; do
    [[ -s "$notices/$name" ]] || fail "the archive has no $name"
done

version="$(basename "$archive")"
version="${version#phux-rune-}"
version="${version%-macos-arm64.zip}"
version="${version%-linux-*}"
[[ "$("${clean[@]}" "$binary" --version)" == "phux-rune $version" ]] \
    || fail "the binary is not version $version"

socket="$scratch/s"
# Directly, so $! is the server itself and cleanup can stop it by that pid.
"${clean[@]}" "$phux" --socket "$socket" server --session "$marker" >"$scratch/server.log" 2>&1 &
server=$!
for _ in $(seq 1 150); do
    [[ ! -S "$socket" ]] || break
    kill -0 "$server" 2>/dev/null || fail "the server exited: $(cat "$scratch/server.log")"
    sleep 0.1
done
[[ -S "$socket" ]] || fail "the server never listened"

snap="$scratch/launch.png"
tree="$scratch/launch.json"
"${clean[@]}" "$binary" --socket "$socket" --session "$marker" \
    --snap "$snap" --tree "$tree" --at 3 --size 1100x720
[[ -s "$snap" ]] || fail "no frame was drawn"
grep -q "$marker" "$tree" || fail "the window never showed session $marker"

if [[ "$(uname -s)" == Linux ]]; then
    command -v xvfb-run >/dev/null || fail "Linux qualification needs xvfb-run"
    # Headless runs skip the windowing path; still running at the deadline
    # (timeout's 124) means it opened and stayed up.
    code=0
    "${clean[@]}" xvfb-run -a timeout 10 "$binary" --socket "$socket" --session "$marker" \
        >"$scratch/window.log" 2>&1 || code=$?
    [[ "$code" == 124 ]] || fail "the window exited $code: $(cat "$scratch/window.log")"
fi
echo "qualify: $(basename "$archive") launched"
