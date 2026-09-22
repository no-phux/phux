#!/usr/bin/env bash
# npm typecheck, unit, pack, and audit gates for the agent integrations.
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

for package in opencode pi claude; do
    npm --prefix "integrations/$package" ci
    npm --prefix "integrations/$package" run gates
done
