#!/usr/bin/env bash
# Stable newline-delimited CLI; Python keeps the dependency map data-driven.
set -euo pipefail
ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
exec python3 "${ROOT}/scripts/ci/classify-changes.py"
