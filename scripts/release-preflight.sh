#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/release-preflight.sh <vX.Y.Z> [--skip-crate-dry-run]

Checks the local release surface before dispatching the GitHub release workflow:
  - workspace package versions match the tag
  - install/release docs still describe the shipped artifact contract
  - Homebrew formula generation still works
  - phux-protocol still packages for crates.io, unless skipped
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

[ "$#" -ge 1 ] || {
  usage >&2
  exit 2
}

tag="$1"
shift

skip_crate_dry_run=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --skip-crate-dry-run)
      skip_crate_dry_run=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      die "unknown option: $1"
      ;;
  esac
done

case "$tag" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *) die "tag must look like vX.Y.Z, got: ${tag}" ;;
esac

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> release version"
bash scripts/check-release-version.sh "$tag"
node scripts/check-agent-integration-versions.mjs

echo "==> install surface"
bash scripts/check-install-surface.sh

echo "==> Homebrew formula generator"
bash scripts/check-formula.sh

if [ "$skip_crate_dry_run" -eq 0 ]; then
  echo "==> phux-protocol crates.io package"
  cargo publish --locked --dry-run --allow-dirty -p phux-protocol
else
  echo "==> phux-protocol crates.io package skipped"
fi

cat <<EOF
release preflight ok: ${tag}

Next:
  1. Land this checkout on the default branch.
  2. Open Actions -> release -> Run workflow.
  3. Enter tag: ${tag}
  4. Set publish_protocol=true only when you want to publish phux-protocol.
EOF
