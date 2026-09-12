#!/usr/bin/env bash
# Embed one POSIX implementation into the two files served directly to `sh`.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mode="${1:---check}"
[[ $mode == --check || $mode == --write ]] || { echo 'usage: sync-install-resolver.sh [--check|--write]' >&2; exit 2; }
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
for installer in install.sh install-cockpit.sh; do
  awk -v source="$ROOT/scripts/lib/install-release.sh" '
    /^# BEGIN shared release resolver$/ {
      starts++; inside = 1; print
      while ((getline line < source) > 0) print line
      close(source); next
    }
    /^# END shared release resolver$/ { ends++; inside = 0 }
    !inside { print }
    END { if (starts != 1 || ends != 1) exit 1 }
  ' "$ROOT/scripts/$installer" > "$tmp"
  if [[ $mode == --write ]]; then
    cat "$tmp" > "$ROOT/scripts/$installer"
  elif ! cmp -s "$tmp" "$ROOT/scripts/$installer"; then
    echo "$installer resolver is stale; run bash scripts/sync-install-resolver.sh --write" >&2
    exit 1
  fi
done
