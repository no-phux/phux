#!/usr/bin/env bash
# Sign release checksums with the phux release key (ADR-0178).
#
#   PHUX_MINISIGN_SECRET_KEY=<minisign secret key file contents> \
#     bash scripts/ci/sign-release.sh dist/<asset>.sha256...
#
# Writes <asset>.sha256.minisig beside each sidecar, then verifies it against
# the keys scripts/install.sh embeds, so a secret no shipped binary trusts
# fails here instead of on every user's machine. The key is unencrypted and
# never touches the workspace.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

die() {
  echo "::error::$*" >&2
  exit 1
}

[[ -n ${PHUX_MINISIGN_SECRET_KEY:-} ]] || die "PHUX_MINISIGN_SECRET_KEY is not set; release checksums must be signed"
[[ $# -gt 0 ]] || die "usage: sign-release.sh <asset>.sha256..."

if ! command -v minisign >/dev/null 2>&1; then
  sudo apt-get update -qq
  sudo apt-get install -y -qq --no-install-recommends minisign >/dev/null
fi

keys="$(sed -n "s/^RELEASE_KEYS='\(.*\)'$/\1/p" "$ROOT/scripts/install.sh")"
[[ -n $keys ]] || die "scripts/install.sh declares no RELEASE_KEYS"

secret="$(mktemp)"
trap 'rm -f "$secret"' EXIT
printf '%s\n' "$PHUX_MINISIGN_SECRET_KEY" > "$secret"

for sidecar in "$@"; do
  [[ $sidecar == *.sha256 && -f $sidecar ]] || die "$sidecar is not a .sha256 sidecar"
  minisign -S -s "$secret" -m "$sidecar" -x "$sidecar.minisig" \
    -t "phux release checksum: $(basename "$sidecar")" </dev/null >/dev/null
  verified=0
  for key in $keys; do
    # -H: prehashed only, the rule phux update and install.sh enforce.
    if minisign -V -H -q -P "$key" -m "$sidecar" -x "$sidecar.minisig" >/dev/null 2>&1; then
      verified=1
      break
    fi
  done
  if [[ $verified -eq 0 ]]; then
    rm -f "$sidecar.minisig"
    die "the signing key is not one scripts/install.sh embeds; see ADR-0178 for rotation"
  fi
  echo "signed $(basename "$sidecar")"
done
