#!/usr/bin/env bash
# Export this job's mbx objects after a failed main lane.
#
# jdx/mr-boxington-action exports from its post step, and that post step
# runs only when the job succeeds. A red assertion does not make the
# finished compilations unusable. The caller saves the directory with
# actions/cache only when this script sets exported=true.
set -euo pipefail

dest="${1:?export directory}"
case "$dest" in
  */phux-mbx-objects) ;;
  *)
    echo "refusing to export to ${dest}" >&2
    exit 2
    ;;
esac

if [[ -z "${MBX_CACHE_EXPORT_GROUP:-}" ]]; then
  echo "mbx export group is unset; not saving"
  exit 0
fi

rm -rf "$dest"
if ! mbx cache export --group "$MBX_CACHE_EXPORT_GROUP" --format directory "$dest"; then
  echo "mbx cache export produced nothing to save"
  exit 0
fi
if [[ ! -d "$dest" ]]; then
  echo "mbx cache export did not write a directory bundle"
  exit 0
fi
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  echo "exported=true" >> "$GITHUB_OUTPUT"
fi
