#!/usr/bin/env bash
set -euo pipefail

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
CLASSIFIER="${ROOT}/scripts/ci/classify-changes.sh"

# Expected outputs: docs_only phux_needed workflow_only cockpit_needed
check() {
  local name="$1" docs="$2" phux="$3" wf="$4" cockpit="$5"
  shift 5
  local output
  output="$(printf '%s\n' "$@" | "${CLASSIFIER}")"
  for pair in "docs_only=${docs}" "phux_needed=${phux}" "workflow_only=${wf}" "cockpit_needed=${cockpit}"; do
    grep -Fxq "${pair}" <<<"${output}" || {
      printf 'error: %s: expected %s\n%s\n' "${name}" "${pair}" "${output}" >&2
      return 1
    }
  done
}

# Empty and unknown input fail closed into the full lanes.
check empty false true false false
check cockpit-source false false false true clients/cockpit/src/main.zig
check cockpit-doc true false false true clients/cockpit/README.md
check cockpit-workflow false false true false .github/workflows/cockpit-ci.yml
check phux-source false true false false crates/phux-server/src/lib.rs
check shared-ffi false true false true crates/phux-client-ffi/src/lib.rs
check shared-cargo false true false true Cargo.lock
check shared-release-config false true false true release-please-config.json
check manifest-only false true false false .release-please-manifest.json
check cockpit-release-metadata false false false true clients/cockpit/version.txt .release-please-manifest.json
check root-doc true true false false docs/RELEASING.md
check mixed false true false true clients/cockpit/src/main.zig crates/phux-protocol/src/lib.rs
# Workflow infrastructure: the gate is actionlint + the pin check + the
# truth tables, never the Rust compile. Dependabot's grouped action bumps
# are exactly this shape.
check workflow-only false false true false .github/workflows/ci.yml
check workflow-bump-batch false false true false .github/workflows/ci.yml .github/workflows/cockpit-ci.yml .github/workflows/linear-release.yml
check composite-manifest false false true false .github/actions/setup-rust-lane/action.yml
check actionlint-config false false true false .github/actionlint.yaml
check workflow-plus-code false true false true .github/workflows/ci.yml crates/phux-protocol/src/lib.rs
check workflow-plus-cockpit false false false true .github/workflows/cockpit-ci.yml clients/cockpit/src/main.zig

printf 'CI change classification passed.\n'
