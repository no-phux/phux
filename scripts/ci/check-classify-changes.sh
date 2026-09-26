#!/usr/bin/env bash
set -euo pipefail
ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 "${ROOT}/scripts/ci/classify-changes-test.py"
python3 "${ROOT}/scripts/ci/test_release_metadata.py"
