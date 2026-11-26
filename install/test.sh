#!/usr/bin/env bash
# Test the install surface: the installers phux.sh serves (scripts/install*,
# generated from install/src), the release manifest every client reads, and
# the tests that drive them. `just install-surface-check` runs this. The static
# checks (POSIX shebangs, manifest-only release discovery, temporary HOME in
# installer tests) are tools/src/policy/install-surface.test.ts.
set -euo pipefail
# shellcheck source=dev/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/../dev/lib/output.sh"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
bash packaging/skills/test-export.sh
bash packaging/test-pack-release.sh
bash install/test-install.sh
bash install/test-install-server-app.sh
bash packaging/test-gen-formula.sh
bash install/test-install-rune.sh
bash install/first-run-timing.sh --self-test
# Real Windows PowerShell 5.1/7 + WSL qualification lives in windows-wsl.yml.
if command -v pwsh >/dev/null 2>&1; then
  pwsh -NoProfile -File install/test-install-windows.ps1
fi
phux_ok "install surface check passed"
