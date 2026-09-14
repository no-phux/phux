#!/usr/bin/env bash
# Package the host-target release binaries into a release tarball + sha256.
# Layout is owned by scripts/pack-release.sh so a locally-seeded tarball is
# indistinguishable from a CI-built one.
#
# Usage: scripts/dist.sh v0.0.1
#
# Produces dist/phux-<tag>-<host-target>.tar.gz (+ .sha256). Assumes the
# release binaries were built by scripts/build-release-binaries.sh; it does not
# build them, so the caller controls the host toolchain.
set -euo pipefail

tag="${1:?usage: dist.sh <tag>  e.g. dist.sh v0.0.1}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

# Host target triple straight from rustc so the artifact name matches what
# the formula expects (aarch64-apple-darwin, x86_64-unknown-linux-gnu, ...).
target="$(rustc -vV | sed -n 's/^host: //p')"

bin_dir="target/release"
receipt="${bin_dir}/.phux-cpu-baseline"
expected_receipt="${target} rust=$(case "$target" in x86_64-unknown-linux-gnu) echo x86-64 ;; aarch64-unknown-linux-gnu) echo generic ;; aarch64-apple-darwin) echo apple-m1 ;; *) echo unsupported ;; esac) libghostty=baseline"
if [ ! -f "$receipt" ] || ! grep -Fxq "$expected_receipt" "$receipt"; then
  echo "error: release CPU baseline receipt is missing or stale — run 'bash scripts/build-release-binaries.sh ${target}' first" >&2
  exit 1
fi

bash scripts/pack-release.sh --id "$tag" --target "$target" --bin-dir "$bin_dir" --out-dir dist
