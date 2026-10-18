#!/usr/bin/env bash
# Exercise the supported umbrella and the actual XCFramework header copier,
# without requiring cross-compiled archives or writing installed locations.
set -euo pipefail

crate_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROOT="$(cd "$crate_dir/../.." && pwd)"
CRATE=phux-client-ffi
# shellcheck source=scripts/lib/ffi-xcframework-artifact.sh
source "$ROOT/scripts/lib/ffi-xcframework-artifact.sh"
die() { echo "$*" >&2; exit 1; }

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
stage="$out/Headers"
stage_ffi_headers "$stage"
verify_ffi_headers "$stage"
diff -r "$crate_dir/include/phux" "$stage/phux"

# Both source and packaged trees must work with only their own include root.
# Include twice to exercise the umbrella and section guards.
for headers in "$crate_dir/include" "$stage"; do
    printf '#include "phux/client.h"\n#include "phux/client.h"\n' |
        "${CC:-cc}" -std=c11 -Wall -Wextra -Werror -Wno-unused-command-line-argument \
            -x c -fsyntax-only -I "$headers" -
    printf '#include "phux/client.h"\n#include "phux/client.h"\n' |
        "${CXX:-c++}" -std=c++17 -Wall -Wextra -Werror -Wno-unused-command-line-argument \
            -x c++ -fsyntax-only -I "$headers" -
done

# The unchanged module map must expose the transitive declarations to importers.
if command -v clang >/dev/null 2>&1; then
    printf '@import PhuxFFI;\nPhuxConnectOptions options;\n' |
        clang -x objective-c -fsyntax-only -fmodules -fmodules-cache-path="$out/modules" \
            -I "$stage" -Wno-unused-command-line-argument -
fi

# Slice verification must reject a missing or altered transitive header.
header=phux/detail/connection.h
rm "$stage/$header"
if (verify_ffi_headers "$stage") >"$out/missing.log" 2>&1; then
    die "slice verification accepted a missing detail header"
fi
cp "$crate_dir/include/$header" "$stage/$header"
printf '\n/* altered package */\n' >> "$stage/$header"
if (verify_ffi_headers "$stage") >"$out/altered.log" 2>&1; then
    die "slice verification accepted an altered detail header"
fi
cp "$crate_dir/include/$header" "$stage/$header"
verify_ffi_headers "$stage"
echo 'ok   umbrella C11/C++17, packaged header tree, module import, missing/altered header refusal'
