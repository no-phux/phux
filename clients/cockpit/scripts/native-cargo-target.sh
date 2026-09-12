#!/usr/bin/env bash
# Shared native build/output contract. Source from a same-checkout artifact owner.

phux_native_cargo_setup() {
    local root="${1:?checkout root required}" profile="${2:?Cargo profile required}" version
    version="$("${RUSTC:-rustc}" -vV)"
    PHUX_CARGO_HOST="$(printf '%s\n' "$version" | sed -n 's/^host: //p')"
    case "$PHUX_CARGO_HOST" in
        ''|*[!a-zA-Z0-9_-]*) printf 'error: rustc did not report a valid native host target\n' >&2; return 1 ;;
    esac
    export CARGO_TARGET_DIR="${root}/target"
    # --target overrides both CARGO_BUILD_TARGET and Cargo config build.target.
    # Cargo then ALWAYS nests outputs under that explicit target triple.
    # Public outputs consumed by both sourcing build scripts.
    # shellcheck disable=SC2034
    CARGO_ARGS=(--locked --manifest-path "${root}/Cargo.toml" --profile "$profile" --target "$PHUX_CARGO_HOST")
    # shellcheck disable=SC2034
    PHUX_CARGO_OUTPUT="${CARGO_TARGET_DIR}/${PHUX_CARGO_HOST}/${profile}"
}

phux_publish_native_artifact() (
    # Keep the existing Zig/CI target/<profile> path contract. Replace the name
    # atomically so retained readers cannot observe a truncated archive or CLI.
    local source="${1:?artifact required}" destination="${2:?destination required}" staged
    mkdir -p -- "$(dirname -- "$destination")"
    staged="$(mktemp "${destination}.XXXXXX")"
    trap 'rm -f -- "$staged"' EXIT
    cp -p -- "$source" "$staged"
    mv -f -- "$staged" "$destination"
)
