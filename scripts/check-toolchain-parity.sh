#!/usr/bin/env bash
# Compare the tools the Nix and Mise environments actually resolve.
#
# check-toolchain-sync.sh is the static, CI-safe half: it reads mise.toml,
# flake.nix and the manifests and checks they AGREE ON PAPER. It cannot see
# what a shell hands you. This is the other half — it realizes both
# environments and compares binary against binary — which is why it is
# advisory and local-only rather than a `just ci` gate: a CI checkout has no
# Mise, and realizing the dev shell to lint a linter version is not a bar to
# hold a PR to.
#
# Run it when bumping a pin, updating flake.lock, or wondering why a gate
# passes for you and fails for someone else. Skips (exit 0) when either
# environment is unavailable, so it is safe to run anywhere.
#
# Node compares on MAJOR only: mise.toml pins the major (`node = "24"`) while
# nixpkgs resolves a specific patch, and that is the intended contract, not
# drift. Every other tool must match exactly.
set -uo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
PROBE="scripts/lib/toolchain-probe.sh"
cd "$ROOT" || exit 1

skip() {
    printf 'toolchain parity: skipped (%s)\n' "$1"
    exit 0
}

command -v nix >/dev/null 2>&1 || skip 'nix not installed'
command -v mise >/dev/null 2>&1 || skip 'mise not installed'

# The dev shell's greeting goes to stdout, so keep only probe-shaped lines
# (`name<TAB>version`) rather than letting a banner become a table row.
probe_lines() { awk -F'\t' 'NF == 2 && $1 ~ /^[a-z][a-z-]*$/'; }

nix_out="$(nix develop -c bash "$PROBE" 2>/dev/null | probe_lines)" || skip 'could not realize the Nix dev shell'
[[ -n "$nix_out" ]] || skip 'the Nix dev shell produced no probe output'
# `mise exec` with no tool arguments provisions everything mise.toml names.
# Strip the ambient environment's PATH influence as far as mise allows, so a
# tool leaking in from the host is not mistaken for one Mise supplied.
mise_out="$(mise exec -- bash "$PROBE" 2>/dev/null | probe_lines)" || skip 'could not resolve the Mise environment'
[[ -n "$mise_out" ]] || skip 'the Mise environment produced no probe output'

failures=0
printf '%-12s %-18s %-18s %s\n' TOOL NIX MISE STATUS
while IFS=$'\t' read -r tool nix_version; do
    mise_version="$(printf '%s\n' "$mise_out" | awk -F'\t' -v t="$tool" '$1 == t { print $2 }')"

    left="$nix_version"
    right="$mise_version"
    if [[ "$tool" == node ]]; then
        left="${left%%.*}"
        right="${right%%.*}"
    fi

    if [[ "$nix_version" == '-' || "$mise_version" == '-' ]]; then
        # Not every tool is meant to exist on both paths; mise.toml's
        # "deliberately absent" section says which. Report, do not fail.
        status='note: absent on one path'
    elif [[ "$left" == "$right" ]]; then
        status='ok'
    else
        status='DRIFT'
        failures=$((failures + 1))
    fi
    printf '%-12s %-18s %-18s %s\n' "$tool" "$nix_version" "$mise_version" "$status"
done <<<"$nix_out"

if [[ "$failures" -ne 0 ]]; then
    printf '\ntoolchain parity: %d tool(s) differ between the Nix and Mise environments.\n' "$failures" >&2
    printf 'Reconcile mise.toml with the flake (or bump flake.lock), then re-run.\n' >&2
    exit 1
fi

printf '\ntoolchain parity: Nix and Mise agree on every shared tool.\n'
