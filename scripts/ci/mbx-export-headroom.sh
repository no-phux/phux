#!/usr/bin/env bash
# Leave disk for the mbx objects-mode post step (phux-gsmcg, phux-2mxuz).
#
# Objects mode sets MBX_GC_AUTO=0 so a restored bundle is not evicted while
# the lane compiles. The action's post step then runs `mbx cache export`
# into a sibling directory and actions/cache compresses that directory. Run
# 38505694112 passed 5491 tests, then the export returned ENOSPC: the store
# was 36.5 GiB, the managed target another 37.3 GiB, and 30 GiB was free.
# ext4 cannot reflink the workspace restore, so those two trees are copies,
# and the export copies the store a third time.
#
# Never run mbx GC here: pending export groups reference the action results
# GC evicts, even when the disk looks empty. Collapse the duplicate target
# bytes onto the store, drop preinstalled toolchains the early disk step
# left behind, and if the copy still cannot fit, retire the pending receipts.
# The post step then records an empty export and the job stays green. The
# restore-key prefix keeps the previous cache entry.
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

# The setup step deletes these only when the disk is already under 40 GiB
# free. The test lane starts near 108 GiB free (run 38505694112) and is full
# after the build, so the same trees are still occupying the export's copy.
if [[ "${GITHUB_ACTIONS:-}" == "true" && -n "${RUNNER_OS:-}" ]]; then
  sudo -n rm -rf \
    /usr/share/dotnet \
    /usr/local/lib/android \
    /opt/ghc \
    /usr/local/.ghcup \
    /opt/hostedtoolcache \
    || echo "mbx-export-headroom: toolchain cleanup skipped" >&2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
if [[ -n "${MBX_CACHE_DIR:-}" ]]; then
  cache_dir="${MBX_CACHE_DIR}"
elif [[ -n "${XDG_CACHE_HOME:-}" ]]; then
  cache_dir="${XDG_CACHE_HOME}/mbx"
else
  cache_dir="${HOME}/.cache/mbx"
fi

if [[ -d "${cache_dir}/targets" && -d "${cache_dir}/actions/cas/v1" ]]; then
  # setup-rust-lane puts the devshell's bun on PATH for the mbx lanes.
  bun "${root}/tools/main.ts" mbx-collapse-target-copies "${cache_dir}" \
    || echo "mbx-export-headroom: collapse failed; continuing" >&2
fi

store="${cache_dir}/actions"
if [[ -d "${store}" ]]; then
  free_kb="$(df -Pk / | awk 'NR == 2 { print $4 }')"
  store_kb="$(du -sk "${store}" | awk '{ print $1 }')"
  if [[ "${free_kb}" =~ ^[0-9]+$ && "${store_kb}" =~ ^[0-9]+$ ]]; then
    # The directory export copies the store. actions/cache then writes a
    # zstd archive at about a quarter of that (6.1 GiB for a 27.8 GiB store
    # on run 38505694112), plus a 1 GiB floor for filesystem overhead.
    need_kb=$((store_kb + store_kb / 4 + 1024 * 1024))
    if ((free_kb < need_kb)); then
      echo "mbx-export-headroom: ${free_kb} KiB free, export needs about ${need_kb} KiB; not saving this store" >&2
      groups="${store}/build-receipts/v1/groups"
      if [[ -d "${groups}" ]]; then
        find "${groups}" -type f -exec rm -f {} + \
          || echo "mbx-export-headroom: could not retire export receipts" >&2
      fi
    fi
  else
    echo "mbx-export-headroom: could not measure free space; leaving the export alone" >&2
  fi
fi

df -h / || true
