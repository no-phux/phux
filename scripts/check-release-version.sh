#!/usr/bin/env bash
set -euo pipefail

tag="${1:?usage: check-release-version.sh <tag>  e.g. v0.0.2}"

case "$tag" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *)
    echo "error: release tag must look like vX.Y.Z, got: ${tag}" >&2
    exit 1
    ;;
esac

version="${tag#v}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

packages=(
  phux
  phux-client
  phux-client-core
  phux-config
  phux-core
  phux-mcp
  phux-protocol
  phux-record
  phux-server
  portable-pty-adopt
)

for package in "${packages[@]}"; do
  pkgid="$(cargo pkgid -p "$package")"
  resolved="${pkgid##*#}"
  resolved="${resolved##*@}"
  if [ "$resolved" != "$version" ]; then
    echo "error: ${package} resolves to ${resolved}, expected ${version} for ${tag}" >&2
    exit 1
  fi
done

echo "release version ok: ${tag}"
