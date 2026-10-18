#!/usr/bin/env bash
# Leave disk for the mbx objects-mode post step (phux-gsmcg, phux-2mxuz).
#
# The post step copies the store into an export directory and actions/cache
# compresses it. On ext4 the managed target is a second copy of the store, so
# run 38505694112 passed its tests and then hit ENOSPC on export. Never run
# mbx GC here: pending export groups reference the results GC evicts. Instead
# link duplicate target bytes to the store, drop preinstalled toolchains, and
# if the copy still cannot fit, retire the pending receipts so the post step
# records an empty export and the job stays green.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != "true" && "${PHUX_MBX_EXPORT_HEADROOM:-}" != "1" ]]; then
  echo "mbx-export-headroom: skipped outside GitHub Actions"
  exit 0
fi

cargo_home="${CARGO_HOME:-${HOME:-}/.cargo}"
# An empty HOME would make this `/.cargo`. Never clean the filesystem root.
case "${cargo_home}" in
  / | /.cargo | //.cargo | [!/]*)
    echo "mbx-export-headroom: refusing to clean an empty Cargo home" >&2
    exit 1
    ;;
esac

df -h / || true
rm -rf "${cargo_home}/registry" "${cargo_home}/git"

# The setup step deletes these only when the disk starts under 40 GiB free.
if [[ "${GITHUB_ACTIONS:-}" == "true" && -n "${RUNNER_OS:-}" ]]; then
  sudo -n rm -rf /usr/share/dotnet /usr/local/lib/android /opt/ghc /usr/local/.ghcup /opt/hostedtoolcache \
    || echo "mbx-export-headroom: toolchain cleanup skipped" >&2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cache_dir="${MBX_CACHE_DIR:-${XDG_CACHE_HOME:-${HOME}/.cache}/mbx}"
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
    # The export copy, a zstd archive at about a quarter of the store, and a
    # 1 GiB floor for filesystem overhead.
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
