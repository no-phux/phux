#!/usr/bin/env bash
# Test the install surface: the installers phux.sh serves (scripts/install*,
# generated from install/src), the release manifest every client reads, and
# the tests that drive them. `just install-surface-check` runs this.
set -euo pipefail
# shellcheck source=dev/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/../dev/lib/output.sh"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
failures=0
fail() {
  phux_error "$*"
  failures=$((failures + 1))
}

# no-phux/website serves these verbatim and refuses one without a POSIX shebang.
for installer in scripts/install.sh scripts/install-desktop.sh; do
  [[ $(head -n 1 "$installer") == '#!/bin/sh' ]] || fail "$installer must start with #!/bin/sh"
done

# Clients discover releases from the channel manifest only (ADR-0176); the
# GitHub API's unauthenticated budget is 60 requests an hour. The one API use
# is API_RELEASES: re-fetching an already-named asset that releases/download
# failed to serve.
for client in scripts/install.sh scripts/install-windows.ps1 crates/phux-client-runtime/src/release.rs \
  crates/phux/src/commands/update clients/rune/src/updates.rs; do
  if grep -rhE 'api\.github\.com|releases/latest' "$client" \
    | grep -vE '^(pub const API_RELEASES: &str = "|API_RELEASES=)https://api\.github\.com/repos/no-phux/phux/releases("|$)' \
    | grep -q .; then
    fail "$client must read the channel manifest, not the GitHub release list"
  fi
done

# install.sh appends to the invoking shell's profile; a test that runs it
# against the real home leaves PATH lines behind (phux-gh85y).
shopt -s nullglob
for test in $(grep -lE 'install(-desktop|-rune)?\.sh|install/src/' install/test-*.sh scripts/test-*.sh); do
  grep -qE '^export HOME="\$TMP' "$test" || fail "$test runs an installer; export HOME=\"\$TMP/...\" first"
done

[[ $failures -eq 0 ]] || { phux_error "install surface check failed"; exit 1; }

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
