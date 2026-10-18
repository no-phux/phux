#!/usr/bin/env bash
# Install the release-pinned compiler under an explicit user-owned directory.
# stdout is ONLY the bin directory, suitable for PATH or GITHUB_PATH.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib/dev-toolchain.sh"

if [[ $# -ne 1 || "$1" == --help ]]; then
    phux_error "usage: bash scripts/install-zig.sh DIRECTORY (prints the compiler directory)"
    exit 2
fi
case "$(uname -s)/$(uname -m)" in
    Darwin/arm64) target=aarch64-macos ;;
    Linux/x86_64) target=x86_64-linux ;;
    Linux/aarch64|Linux/arm64) target=aarch64-linux ;;
    *) phux_error "No release-pinned Zig archive for this host. Install Zig $ZIG_VERSION manually; see docs/development.md."; exit 1 ;;
esac
sha="$(zig_digest "$target")"
[[ "$sha" =~ ^[0-9a-f]{64}$ ]] || { phux_error "missing Zig digest for $target"; exit 1; }
mkdir -p "$1"
base="$(cd "$1" && pwd)"
archive="zig-${target}-${ZIG_VERSION}.tar.xz"
dest="$base/${archive%.tar.xz}"
if [[ -e "$dest" ]]; then
    # An existing installation is never overwritten or recursively deleted.
    [[ -x "$dest/zig" && "$("$dest/zig" version)" == "$ZIG_VERSION" ]] || {
        phux_error "invalid existing Zig installation: $dest; choose a different DIRECTORY"
        exit 1
    }
else
    stage="$(mktemp -d "$base/.zig-download.XXXXXX")"
    trap 'rm -rf "$stage"' EXIT
    phux_info "installing Zig $ZIG_VERSION ($target)"
    curl -fSL --retry 3 --connect-timeout 20 --max-time 600 \
        "https://ziglang.org/download/${ZIG_VERSION}/${archive}" -o "$stage/$archive" >&2
    (
        cd "$stage"
        if command -v sha256sum >/dev/null 2>&1; then
            printf '%s  %s\n' "$sha" "$archive" | sha256sum -c - >&2
        else
            printf '%s  %s\n' "$sha" "$archive" | shasum -a 256 -c - >&2
        fi
        tar -xf "$archive"
    )
    [[ "$("$stage/${archive%.tar.xz}/zig" version)" == "$ZIG_VERSION" ]]
    mv "$stage/${archive%.tar.xz}" "$dest"
fi
printf '%s\n' "$dest"
