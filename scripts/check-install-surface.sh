#!/usr/bin/env bash
# The install surface: the installers phux.sh serves, the release manifest
# every client reads, and the tests that drive them.
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"
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
  grep -Fq -- "- \"$installer\"" .github/workflows/website-dispatch.yml \
    || fail "website-dispatch.yml must rebuild phux.sh when $installer changes"
done

# Clients discover releases from the channel manifest only (ADR-0176); the
# GitHub API's unauthenticated budget is 60 requests an hour.
for client in scripts/install.sh crates/phux-client-runtime/src/release.rs \
  crates/phux/src/commands/update clients/desktop/native/src/updates.rs; do
  if grep -rEq 'api\.github\.com|releases/latest' "$client"; then
    fail "$client must read the channel manifest, not the GitHub release list"
  fi
done

[[ $failures -eq 0 ]] || { phux_error "install surface check failed"; exit 1; }

python3 scripts/ci/release_manifest.py fixtures --check
bash scripts/check-product-skills-export.sh
bash scripts/test-pack-release.sh
bash scripts/test-install.sh
python3 scripts/test-install-desktop.py
phux_ok "install surface check passed"
