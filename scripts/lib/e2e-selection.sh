#!/usr/bin/env bash
# Sourced by the archive build and e2e runner. These are nextest binary IDs;
# Cargo's --test names are their suffixes. Keep them unique across packages.
E2E_BINARIES=(
    phux::automation_e2e phux::terminal_e2e phux::recording_e2e
    phux::lifecycle_e2e phux::first_five_minutes_e2e
    phux::upload_e2e phux::download_e2e phux::git_e2e phux::credentials_e2e
    phux-server::perf
)

e2e_select() {
    local spec pkg bin id specs selected=()
    if [[ -n "${PHUX_UNIT_TARGETS:-}" ]]; then
        IFS=',' read -ra specs <<< "$PHUX_UNIT_TARGETS"
        for spec in "${specs[@]}"; do
            IFS=':' read -r pkg bin _ <<< "$spec"
            for id in "${E2E_BINARIES[@]}"; do
                if [[ "$id" == "$pkg::$bin" ]]; then
                    selected+=("$id")
                    break
                fi
            done
        done
    fi
    # Narrow only when every requested target maps to this lane. A mixed
    # selection may contain an e2e consumer outside this list.
    # Read by just e2e after selection.
    # shellcheck disable=SC2034
    e2e_partition_extra=(--no-tests=warn)
    if [[ -z "${selected[*]:-}" ]] || [[ "${#selected[@]}" -ne "${#specs[@]}" ]]; then
        # shellcheck disable=SC2034
        e2e_partition_extra=()
        selected=("${E2E_BINARIES[@]}")
    fi
    # discovery is not executed here. It makes Cargo build phux-mcp for
    # the smoke tests that launch the same-checkout executable.
    e2e_targets=(--test discovery)
    e2e_filter=
    for id in "${selected[@]}"; do
        e2e_targets+=(--test "${id#*::}")
        e2e_filter="${e2e_filter:+${e2e_filter} + }binary_id($id)"
    done
}
