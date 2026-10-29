#!/usr/bin/env bash
# Fail when the generator lock's `toml` crate is not the same version as the
# root lock. The generator is its own workspace, so the two locks can drift.
set -euo pipefail
root=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)

toml_version() {
    awk '
        $0 == "name = \"toml\"" { hit = 1; next }
        hit && $1 == "version" { gsub(/"/, "", $3); print $3; exit }
        hit && /^\[/ { hit = 0 }
    ' "$1"
}

root_ver=$(toml_version "$root/Cargo.lock")
gen_ver=$(toml_version "$root/scripts/plugin-index/Cargo.lock")
if [[ -z "$root_ver" || "$root_ver" != "$gen_ver" ]]; then
    printf 'toml version drifted: root=%s generator=%s\n' "${root_ver:-missing}" "${gen_ver:-missing}" >&2
    exit 1
fi
