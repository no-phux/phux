#!/usr/bin/env bash
# Classify a newline-delimited changed-file list for the required Phux CI jobs.
# Empty input and unknown root paths fail closed into the full Phux lanes.
set -euo pipefail

mapfile -t files

docs_only=true
phux_needed=false
workflow_only=true
cockpit_needed=false
seen_file=false
seen_cockpit_file=false
seen_release_manifest=false

for file in "${files[@]}"; do
  [[ -n "${file}" ]] || continue
  seen_file=true

  case "${file}" in
    skills/*)
      docs_only=false
      ;;
    docs/*|ADR/*|*.md)
      ;;
    *)
      docs_only=false
      ;;
  esac

  case "${file}" in
    # Workflow infrastructure is verified by the workflow gate (actionlint,
    # the SHA-pin check, the path-routing truth table, and the release-
    # orchestration/drift assertions), never by recompiling the workspace.
    # This is what keeps Dependabot's grouped action bumps — which touch
    # nothing but workflow files — off the zig blob. An action bump changes
    # runtime steps, not compilation, and the next code PR exercises the
    # lanes against the new version. Anything OUTSIDE this set fails closed
    # into full lanes as before.
    .github/workflows/*.yml|.github/actions/**/*.yml|.github/actionlint.yaml)
      ;;
    *)
      workflow_only=false
      case "${file}" in
        # Cockpit owns its complete subtree; its dedicated workflows live in
        # the workflow-owned set above and are gated separately.
        clients/cockpit/*)
          seen_cockpit_file=true
          cockpit_needed=true
          ;;
        # Shared inputs cockpit's lanes compile against: the FFI/protocol/
        # perf crates and the workspace-level files its build resolves.
        crates/phux-client-core/*|crates/phux-client-ffi/*|crates/phux-perf/*|crates/phux-protocol/*|.cargo/config.toml|Cargo.lock|Cargo.toml|rust-toolchain.toml|justfile|release-please-config.json)
          phux_needed=true
          cockpit_needed=true
          ;;
        .release-please-manifest.json)
          # A Cockpit Release Please PR changes this root file alongside the
          # component subtree. Defer it so that exact shape stays Cockpit-only;
          # a manifest-only edit still fails closed below.
          seen_release_manifest=true
          ;;
        *)
          phux_needed=true
          ;;
      esac
      ;;
  esac
done

if [[ "${seen_file}" == "false" ]]; then
  docs_only=false
  phux_needed=true
  workflow_only=false
fi

if [[ "${seen_release_manifest}" == "true" && "${seen_cockpit_file}" == "false" ]]; then
  phux_needed=true
fi

printf 'docs_only=%s\n' "${docs_only}"
printf 'phux_needed=%s\n' "${phux_needed}"
printf 'workflow_only=%s\n' "${workflow_only}"
printf 'cockpit_needed=%s\n' "${cockpit_needed}"
