#!/usr/bin/env bash
# Embed shared POSIX implementations into files served directly to `sh`.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/output.sh
source "$ROOT/scripts/lib/output.sh"
mode="${1:---check}"
[[ $mode == --check || $mode == --write ]] || { phux_error 'usage: sync-install-resolver.sh [--check|--write]'; exit 2; }
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
for installer in install.sh install-cockpit.sh install-desktop.sh; do
  for library in output install-release; do
    case "$library" in
      output) marker='CLI output' ;;
      install-release) marker='release resolver' ;;
    esac
    awk -v source="$ROOT/scripts/lib/$library.sh" -v marker="$marker" '
      $0 == "# BEGIN shared " marker {
        starts++; inside = 1; print
        while ((getline line < source) > 0) print line
        close(source); next
      }
      $0 == "# END shared " marker { ends++; inside = 0 }
      !inside { print }
      END { if (starts != 1 || ends != 1) exit 1 }
    ' "$ROOT/scripts/$installer" > "$tmp"
    if [[ $mode == --write ]]; then
      cp "$tmp" "$ROOT/scripts/$installer"
    elif ! cmp -s "$tmp" "$ROOT/scripts/$installer"; then
      phux_error "$installer $marker is stale; run bash scripts/sync-install-resolver.sh --write"
      exit 1
    fi
  done
done
