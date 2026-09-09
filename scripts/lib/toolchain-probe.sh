#!/usr/bin/env bash
# Print `tool<TAB>version` for every tool the Nix and Mise environments are
# both expected to supply. Run this INSIDE an environment, not against one:
# check-toolchain-parity.sh executes this identical file in each and compares
# the two outputs, so neither side can be measured by a different rule than the
# other. A tool that is absent prints `-` rather than failing, because a
# missing tool is a finding for the comparator to report, not a crash here.
set -uo pipefail

probe() {
    local name="$1" bin="$2" cmd="$3" version
    if ! command -v "$bin" >/dev/null 2>&1; then
        printf '%s\t-\n' "$name"
        return
    fi
    version="$(bash -c "$cmd" 2>/dev/null || true)"
    printf '%s\t%s\n' "$name" "${version:--}"
}

probe rust       rustc      'rustc --version | cut -d" " -f2'
probe zig        zig        'zig version'
probe node       node       'node --version | tr -d v'
probe bun        bun        'bun --version'
probe just       just       'just --version | cut -d" " -f2'
probe cargo-deny cargo-deny 'cargo-deny --version | cut -d" " -f2'
probe actionlint actionlint 'actionlint --version | head -1'
probe shellcheck shellcheck 'shellcheck --version | sed -n "s/^version: //p"'
probe jq         jq         'jq --version | sed "s/^jq-//"'
probe python     python3    'python3 --version | cut -d" " -f2'
