#!/usr/bin/env bash
# Keep checkout paths out of compiled library and test code.
#
# env!("CARGO_MANIFEST_DIR") and env!("CARGO_BIN_EXE_*") compile the current
# checkout's absolute path into the crate. A content-addressed build cache
# (mbx, docs/SETUP.md#build-cache-mbx) then keys that crate, and every crate
# that links it, to one checkout, so each new worktree recompiles them instead
# of restoring them: before this gate, phux-config's one baked path rebuilt
# phux-server and everything above it in every fresh worktree.
#
# Read the value at run time instead. Tests: `cargo test` and nextest both
# export CARGO_MANIFEST_DIR, and CARGO_BIN_EXE_<name> / NEXTEST_BIN_EXE_<name>
# (crates/phux/tests/common/runner.rs). Binaries: pass the path in from a bin
# target, which nothing links (crates/phux/src/main.rs).
#
# Allowed: bin targets and examples, which are leaves. insta's snapshot macros
# expand env!("CARGO_MANIFEST_DIR") themselves and are not matched here.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

if hits="$(git grep -n -E 'env!\("(CARGO_MANIFEST_DIR|CARGO_BIN_EXE_[^"]+)"\)' -- \
    'crates/**/*.rs' \
    ':!crates/*/examples/**' \
    ':!crates/*/src/main.rs' \
    ':!crates/*/src/bin/**')"; then
    printf '%s\n' "$hits" >&2
    printf '\ncache-portable: the lines above compile a checkout path into a library or test.\n' >&2
    printf 'Read it at run time instead; see the comment in scripts/check-cache-portable.sh.\n' >&2
    exit 1
fi
printf 'cache-portable: no compile-time checkout paths in library or test code\n'
