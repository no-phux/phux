#!/usr/bin/env bash
# Hermetic tests for generate-operation-fixtures.sh rlib selection.
#
# Watched red: after a crate-version bump, cargo leaves the previous
# libphux_protocol-<hash>.rlib beside the new one, and
# [[ ${#protocol[@]} == 1 ]] fails (phux-pfyn, 0.37 -> 0.38).
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/generate-operation-fixtures.sh
source "${ROOT}/scripts/generate-operation-fixtures.sh"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/operation-fixtures-test.XXXXXX")"
trap 'rm -rf -- "$WORK"' EXIT

pass=0
fail=0
ok() { pass=$((pass + 1)); printf 'ok: %s\n' "$1"; }
bad() { printf 'FAIL: %s\n' "$1" >&2; fail=$((fail + 1)); }

# Two metadata-hash rlibs, as cargo leaves them after a version bump.
touch -t 202001010000 "$WORK/libphux_protocol-oldhash.rlib"
touch -t 202601010000 "$WORK/libphux_protocol-newhash.rlib"
touch -t 202001010000 "$WORK/libbytes-oldhash.rlib"
touch -t 202601010000 "$WORK/libbytes-newhash.rlib"

old_protocol=("$WORK"/libphux_protocol-*.rlib)
old_bytes=("$WORK"/libbytes-*.rlib)
if [[ ${#old_protocol[@]} == 1 || ${#old_bytes[@]} == 1 ]]; then
    bad 'fixture did not reproduce the stale second rlib'
else
    ok 'stale second rlib fixture (the old exact-count assert fails here)'
fi

got="$(newest_rlib "$WORK"/libphux_protocol-*.rlib)"
if [[ "$got" == "$WORK/libphux_protocol-newhash.rlib" ]]; then
    ok 'newest_rlib picks the newer protocol rlib'
else
    bad "newest_rlib protocol: got $got"
fi

got="$(newest_rlib "$WORK"/libbytes-*.rlib)"
if [[ "$got" == "$WORK/libbytes-newhash.rlib" ]]; then
    ok 'newest_rlib picks the newer bytes rlib'
else
    bad "newest_rlib bytes: got $got"
fi

got="$(newest_rlib "$WORK/libphux_protocol-newhash.rlib")"
if [[ "$got" == "$WORK/libphux_protocol-newhash.rlib" ]]; then
    ok 'newest_rlib accepts a single rlib'
else
    bad "single rlib: got $got"
fi

if newest_rlib "$WORK/libphux_protocol-missing.rlib" >/dev/null 2>&1; then
    bad 'missing rlib was accepted'
else
    ok 'newest_rlib refuses a missing rlib'
fi

if [[ "$fail" -ne 0 ]]; then
    printf '%s passed, %s failed\n' "$pass" "$fail" >&2
    exit 1
fi
printf '%s passed\n' "$pass"
