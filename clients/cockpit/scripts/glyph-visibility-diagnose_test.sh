#!/usr/bin/env bash
# Focused fixtures for scripts/glyph-visibility-diagnose.sh.
#
# These checks cover the distinction that matters: an old version is stale,
# while a same-version digest difference is only a different build until build
# provenance exists. The fixtures never launch an app and never touch /Applications.
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
DIAG="${ROOT}/scripts/glyph-visibility-diagnose.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/glyph-diagnose-test.XXXXXX")"
trap 'rm -rf -- "$WORK"' EXIT

version="$(sed -n '1p' "${ROOT}/version.txt")"

make_bundle() {
    local bundle="$1" bundle_version="$2" bundle_id="$3" executable="$4" payload="$5"
    mkdir -p -- "${bundle}/Contents/MacOS"
    cat >"${bundle}/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleShortVersionString</key><string>${bundle_version}</string>
<key>CFBundleVersion</key><string>${bundle_version}</string>
<key>CFBundleIdentifier</key><string>${bundle_id}</string>
<key>CFBundleName</key><string>Fixture</string>
<key>CFBundleExecutable</key><string>${executable}</string>
</dict></plist>
EOF
    printf '%s' "$payload" >"${bundle}/Contents/MacOS/${executable}"
    chmod +x "${bundle}/Contents/MacOS/${executable}"
}

assert_contains() {
    local label="$1" output="$2" expected="$3"
    [[ "$output" == *"$expected"* ]] || {
        printf 'FAIL: %s\nexpected: %s\noutput:\n%s\n' "$label" "$expected" "$output" >&2
        exit 1
    }
    printf 'ok: %s\n' "$label"
}

assert_refused() {
    local label="$1" expected="$2" output
    shift 2
    if output="$("$@" 2>&1)"; then
        printf 'FAIL: %s unexpectedly succeeded\noutput:\n%s\n' "$label" "$output" >&2
        exit 1
    fi
    assert_contains "$label" "$output" "$expected"
}

DEV="${WORK}/dev.app"
make_bundle "$DEV" "$version" "dev.fixture" "phux-cockpit-dev" "current-build"

output="$($DIAG --installed "${WORK}/missing.app" --dev "$DEV")"
assert_contains absent-installed "$output" 'stale binary: UNKNOWN (installed bundle metadata or executable is missing)'

OLD="${WORK}/old.app"
make_bundle "$OLD" "0.1.0" "old.fixture" "old" "old-build"
output="$($DIAG --installed "$OLD" --dev "$DEV")"
assert_contains version-mismatch "$output" 'stale binary: CONFIRMED (installed version differs from this source)'

DIFFERENT="${WORK}/different.app"
make_bundle "$DIFFERENT" "$version" "different.fixture" "different" "different-build"
output="$($DIAG --installed "$DIFFERENT" --dev "$DEV")"
assert_contains same-version-different-build "$output" 'stale binary: DIFFERENT BUILD (same version, executable digest differs; stale source/config unresolved)'

MATCHING="${WORK}/matching.app"
make_bundle "$MATCHING" "$version" "matching.fixture" "dev" "current-build"
output="$($DIAG --installed "$MATCHING" --dev "$DEV")"
assert_contains same-digest "$output" 'stale binary: NOT SHOWN (installed version and executable digest match this dev bundle)'

TRAVERSAL="${WORK}/traversal.app"
make_bundle "$TRAVERSAL" "$version" "traversal.fixture" "../escape" "outside"
assert_refused traversal-executable 'error: '"$TRAVERSAL"' has an unsafe CFBundleExecutable=../escape' \
    "$DIAG" --installed "$TRAVERSAL" --dev "$DEV"

SYMLINK="${WORK}/symlink.app"
make_bundle "$SYMLINK" "$version" "symlink.fixture" "phux-cockpit-dev" "placeholder"
rm -f -- "$SYMLINK/Contents/MacOS/phux-cockpit-dev"
printf '%s' "outside-target" >"${WORK}/outside-target"
chmod +x "${WORK}/outside-target"
ln -s "${WORK}/outside-target" "$SYMLINK/Contents/MacOS/phux-cockpit-dev"
assert_refused symlink-escape 'executable resolves outside Contents/MacOS' \
    "$DIAG" --installed "$SYMLINK" --dev "$DEV"

MALFORMED_SNAPSHOT="${WORK}/malformed-snapshot.txt"
printf 'publisher_pid=1234 gpu_present_path=pixels gpu_nonblank=true\n' >"$MALFORMED_SNAPSHOT"
assert_refused snapshot-shape 'snapshot:    REFUSED (missing ready header or Cockpit GPU canvas)' \
    "$DIAG" --installed "$MATCHING" --dev "$DEV" --snapshot "$MALFORMED_SNAPSHOT" --pid 1234

SNAPSHOT="${WORK}/snapshot.txt"
printf '%s\n' \
    'ready=true protocol=0x1 frame=1 commands=0 publisher_pid=1234' \
    '  view @w1/phux-cockpit-canvas kind=gpu_surface gpu_present_path=packet present_fallback=none gpu_nonblank=true' \
    >"$SNAPSHOT"
assert_refused snapshot-needs-pid 'error: --snapshot requires --pid with the PID launched for that snapshot' \
    "$DIAG" --installed "$MATCHING" --dev "$DEV" --snapshot "$SNAPSHOT"
assert_refused snapshot-pid-mismatch 'snapshot:    REFUSED (publisher_pid=1234, expected=4321)' \
    "$DIAG" --installed "$MATCHING" --dev "$DEV" --snapshot "$SNAPSHOT" --pid 4321
output="$($DIAG --installed "$MATCHING" --dev "$DEV" --snapshot "$SNAPSHOT" --pid 1234)"
assert_contains snapshot-pid-match "$output" 'display raster: HOST PATH ACTIVE; CoreText output still needs a real-pixel capture'
