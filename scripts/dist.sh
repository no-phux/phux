#!/usr/bin/env bash
# Package the host-target release binaries into a release tarball + sha256,
# matching .github/workflows/release.yml's layout exactly so a locally-seeded
# first release is indistinguishable from a CI-built one.
#
# Usage: scripts/dist.sh v0.0.1
#
# Produces dist/phux-<tag>-<host-target>.tar.gz (+ .sha256). Assumes the
# release binaries are already built (`cargo build --release --bin phux
# --bin phux-mcp`); it does not build them, so the caller controls the
# toolchain (run inside `nix develop`).
set -euo pipefail

tag="${1:?usage: dist.sh <tag>  e.g. dist.sh v0.0.1}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

# Host target triple straight from rustc so the artifact name matches what
# the formula expects (aarch64-apple-darwin, x86_64-unknown-linux-gnu, ...).
target="$(rustc -vV | sed -n 's/^host: //p')"

bin_dir="target/release"
for b in phux phux-mcp; do
  if [ ! -x "${bin_dir}/${b}" ]; then
    echo "error: ${bin_dir}/${b} not found — run 'cargo build --release --bin phux --bin phux-mcp' first" >&2
    exit 1
  fi
done

stage="phux-${tag}-${target}"
out="dist"
rm -rf "${out:?}/${stage}"
mkdir -p "${out}/${stage}"
cp "${bin_dir}/phux" "${bin_dir}/phux-mcp" "${out}/${stage}/"
cp README.md LICENSE NOTICE THIRD-PARTY-NOTICES.md "${out}/${stage}/"

tar -czf "${out}/${stage}.tar.gz" -C "${out}" "${stage}"
rm -rf "${out:?}/${stage}"

if command -v sha256sum >/dev/null; then
  sha=$(sha256sum "${out}/${stage}.tar.gz" | cut -d' ' -f1)
else
  sha=$(shasum -a 256 "${out}/${stage}.tar.gz" | cut -d' ' -f1)
fi
echo "${sha}  ${stage}.tar.gz" > "${out}/${stage}.tar.gz.sha256"

echo "packaged ${out}/${stage}.tar.gz"
echo "sha256   ${sha}"
