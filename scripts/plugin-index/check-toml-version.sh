#!/usr/bin/env bash
# The generator is its own workspace and path-depends on phux-config, so its
# lock is not updated when the product workspace adds or bumps a shared crate.
# Fail when that lock is stale under --locked, and when its `toml` crate is
# not the same version as the root lock.
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
failed=0
if [[ -z "$root_ver" || "$root_ver" != "$gen_ver" ]]; then
    printf 'toml version drifted: root=%s generator=%s\n' "${root_ver:-missing}" "${gen_ver:-missing}" >&2
    failed=1
fi

manifest="$root/scripts/plugin-index/Cargo.toml"
if ! cargo metadata --locked --format-version 1 --manifest-path "$manifest" >/dev/null; then
    cat >&2 <<EOF
scripts/plugin-index/Cargo.lock is stale under --locked.
The generator is a separate workspace and path-depends on phux-config, so a
crate added or bumped in that graph (for example via phux-protocol) is not
written into this lock by the root workspace. Refresh it, then pin toml back
to the root lock's version so the two stay aligned:

  cargo update --manifest-path scripts/plugin-index/Cargo.toml
  cargo update --manifest-path scripts/plugin-index/Cargo.toml -p toml --precise ${root_ver:-<root toml version>}

Do not drop --locked.
EOF
    failed=1
fi

exit "$failed"
