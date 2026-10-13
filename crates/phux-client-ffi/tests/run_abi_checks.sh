#!/usr/bin/env bash
# Compile every tests/*_abi.c against include/phux/client.h, link it against
# the static archive `just cockpit-ffi` built, and run it. The static asserts
# pin the C layouts Cockpit compiles against; the runs pin the refusals an
# unconnected client gives.
#
# Usage: run_abi_checks.sh [PROFILE]   (default ffi-dev)
set -euo pipefail

profile="${1:-ffi-dev}"
crate_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
root="$(cd "$crate_dir/../.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$root/target}"
archive="$target_dir/$profile/libphux_client_ffi.a"
if [[ ! -f "$archive" ]]; then
    echo "missing $archive; run just cockpit-ffi first" >&2
    exit 1
fi

link=()
case "$(uname -s)" in
    Darwin) link=(-framework CoreFoundation -framework Security -framework SystemConfiguration) ;;
    *) link=(-lpthread -ldl -lm) ;;
esac

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
status=0
for source in "$crate_dir"/tests/*_abi.c; do
    name="$(basename "$source" .c)"
    flags=(-std=c11 -Wall -Wextra -Werror -I "$crate_dir/include")
    if ! grep -q 'int main' "$source"; then
        # Static asserts only: compiling is the check.
        if cc "${flags[@]}" -fsyntax-only "$source"; then
            echo "ok   $name"
        else
            echo "FAIL $name" >&2
            status=1
        fi
        continue
    fi
    if cc -std=c11 -Wall -Wextra -Werror -I "$crate_dir/include" "$source" "$archive" \
        "${link[@]}" -o "$out/$name" && "$out/$name"; then
        echo "ok   $name"
    else
        echo "FAIL $name" >&2
        status=1
    fi
done
exit "$status"
