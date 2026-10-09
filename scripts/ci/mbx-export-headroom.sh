#!/usr/bin/env bash
# Leave disk for the mbx objects-mode post step (phux-gsmcg).
#
# Objects mode sets MBX_GC_AUTO=0 so a restored bundle is not evicted while
# the lane compiles. The action's post step then runs `mbx cache export` into
# a directory beside the action store and actions/cache archives that
# directory. A full workspace test filled the hosted runner, the export
# returned ENOSPC, and the green test job failed in post.
#
# The Cargo registry is not part of an objects bundle, so it can be removed
# after the build. Never run mbx GC here: pending export groups reference the
# action results GC evicts, even when there is plenty of disk space. Bound the
# restored store in setup-rust-lane before the first build records that group.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != "true" && "${PHUX_MBX_EXPORT_HEADROOM:-}" != "1" ]]; then
  echo "mbx-export-headroom: skipped outside GitHub Actions"
  exit 0
fi

cargo_home="${CARGO_HOME:-${HOME:-}/.cargo}"
# An empty HOME would make this `/.cargo`. Never clean the filesystem root.
if [[ "${cargo_home}" != /* || "${cargo_home}" == "/" || "${cargo_home}" == "/.cargo" || "${cargo_home}" == "//.cargo" ]]; then
  echo "mbx-export-headroom: refusing to clean an empty Cargo home" >&2
  exit 1
fi

command -v df >/dev/null

df -h / || true
rm -rf "${cargo_home}/registry" "${cargo_home}/git"

df -h / || true
