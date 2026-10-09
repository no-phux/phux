#!/usr/bin/env bash
# Leave disk for the mbx objects-mode post step (phux-gsmcg).
#
# Objects mode sets MBX_GC_AUTO=0 so a restored bundle is not evicted while
# the lane compiles. The action's post step then runs `mbx cache export` into
# a directory beside the action store and actions/cache archives that
# directory. A full workspace test filled the hosted runner, the export
# returned ENOSPC, and the green test job failed in post.
#
# The Cargo registry is not part of an objects bundle. The action store is.
# Collect it to one lane's closure after the build, and once more, lower, if
# that still leaves under 8 GiB free for the second copy. LRU eviction drops
# restored objects this job did not use before it drops the closure the
# export is about to copy. This does not delete the action store.
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

command -v mbx >/dev/null
command -v df >/dev/null

df -h / || true
rm -rf "${cargo_home}/registry" "${cargo_home}/git"

# One lane's saved entry is about 0.4 GiB (check) or 1.9 GiB (test)
# compressed. 8 GiB of objects keeps that closure; 4 GiB is the floor when
# the runner still cannot hold a second copy.
mbx gc --max-size 8GiB
free_kb="$(df -Pk / | awk 'NR == 2 { print $4 }')"
if [[ ! "${free_kb}" =~ ^[0-9]+$ ]]; then
  echo "mbx-export-headroom: df did not report free space: ${free_kb}" >&2
  exit 1
fi
reserve_kb=$((8 * 1024 * 1024))
if (( free_kb < reserve_kb )); then
  mbx gc --max-size 4GiB
fi

df -h / || true
