#!/usr/bin/env bash
# Enforce the action-pinning policy for .github/workflows/: every remote
# action is referenced by full commit SHA with a trailing `# v<tag>` comment.
#
# Tags are mutable — a retagged release swaps the code every workflow runs
# with no diff in this repository. The commit SHA is immutable, and the
# trailing version comment keeps the pin human-auditable (and lets
# Dependabot propose bumps). Local reusable workflows (`uses: ./.github/...`)
# are exempt: they are this tree, reviewed in the same diff.
#
# The cockpit-imported workflows already pinned this way; the phux ones
# floated on major tags, so a compromise of any action maintainer's tag
# would have landed code here silently. One policy, enforced, for all of
# them. Runs in `just workflow-check`.
set -euo pipefail
# shellcheck source=scripts/lib/output.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/output.sh"

ROOT="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
fail=0

while IFS= read -r line; do
  file="${line%%:*}"
  lineno="${line#*:}"; lineno="${lineno%%:*}"
  ref="${line##*uses: }"
  ref="${ref%%[[:space:]]*}"

  case "${ref}" in
    ./*|docker://*)
      continue
      ;;
  esac

  sha="${ref##*@}"
  if ! [[ "${sha}" =~ ^[0-9a-f]{40}$ ]]; then
    phux_error "${file}:${lineno}: action is not SHA-pinned: ${ref}"
    fail=1
    continue
  fi

  # The pin must carry its tag as a trailing comment so a reader (and
  # Dependabot) can tell what the SHA means without resolving it.
  if [[ "$(sed -n "${lineno}p" "${file}")" != *"# v"* ]]; then
    phux_error "${file}:${lineno}: SHA pin missing trailing \"# v<tag>\" comment: ${ref}"
    fail=1
  fi
done < <(grep -Hn '^[[:space:]]*-\{0,1\}[[:space:]]*uses: ' \
         "${ROOT}"/.github/workflows/*.yml "${ROOT}"/.github/actions/*/action.yml \
         | sed 's/^\([^:]*:[0-9]*\):[[:space:]]*-\{0,1\}[[:space:]]*uses: /\1:uses: /')

if [[ "${fail}" -ne 0 ]]; then
  phux_hint 'Pin format: uses: owner/repo@<40-hex-sha> # v<tag>. Resolve with: git ls-remote https://github.com/<owner>/<repo> "refs/tags/<tag>^{}"'
  exit 1
fi

phux_ok 'action pins passed'
