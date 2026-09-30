#!/usr/bin/env bash
# Native harness typecheck, unit, loader, pack, and npm audit gates.
# `just agent-integrations-check` and ci.yml's integrations job both call
# this so the required lane cannot drift from the local recipe. Runtime
# is first: pi bundles it, and the committed dist must match a fresh build.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"

bash scripts/doctor.sh integrations
node scripts/check-agent-integration-versions.mjs

# Incidental install audits are off; the explicit audit inside each
# package's gates script still runs. Same switch as integration-check.
export npm_config_audit=false
export npm_config_fund=false

npm --prefix integrations/runtime ci
npm --prefix integrations/runtime run gates
git diff --exit-code -- integrations/runtime/dist

(
    cd integrations/opencode-v2
    bun install --frozen-lockfile
    bun run gates
)
git diff --exit-code -- integrations/opencode-v2/index.js

(
    cd integrations/omp
    bun install --frozen-lockfile
    bun run gates
)

for package in pi claude; do
    npm --prefix "integrations/$package" ci
    npm --prefix "integrations/$package" run gates
done
