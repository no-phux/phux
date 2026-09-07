#!/usr/bin/env bash
# Replace by rename: a coordinator may still be executing the previous inode.
set -euo pipefail
SOURCE="${1:?source CLI required}"
DESTINATION="${2:?destination required}"
mkdir -p -- "$(dirname -- "$DESTINATION")"
STAGED="$(mktemp "${DESTINATION}.XXXXXX")"
trap 'rm -f -- "$STAGED"' EXIT
cp -- "$SOURCE" "$STAGED"
chmod 755 "$STAGED"
mv -f -- "$STAGED" "$DESTINATION"
