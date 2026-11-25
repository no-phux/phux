#!/usr/bin/env bash
# The install surface: the installers phux.sh serves, the release manifest
# every client reads, and the tests that drive them. The static checks (POSIX
# shebangs, manifest-only release discovery, temporary HOME in installer
# tests) are tools/src/policy/install-surface.test.ts.
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
bash scripts/check-product-skills-export.sh
bash scripts/test-pack-release.sh
bash scripts/test-install.sh
bash scripts/test-install-server-app.sh
bash scripts/test-gen-formula.sh
bash scripts/test-install-rune.sh
bash scripts/first-run-timing.sh --self-test
# Real Windows PowerShell 5.1/7 + WSL qualification lives in windows-wsl.yml.
if command -v pwsh >/dev/null 2>&1; then
  pwsh -NoProfile -File scripts/test-install-windows.ps1
fi
phux_ok "install surface check passed"
