#!/usr/bin/env bash
# Package phux + phux-mcp + the release docs into phux-<id>-<target>.tar.gz
# and a matching .sha256 sidecar.
#
# This is the only writer of that member list. release.yml, next-release.yml,
# and dist.sh call it; the install tests pack stub binaries the same way so
# a packaging change cannot silently diverge from what curl | sh accepts.
set -euo pipefail

RELEASE_BINS=(phux phux-mcp)
RELEASE_DOCS=(README.md LICENSE NOTICE THIRD-PARTY-NOTICES.md)

usage() {
  cat <<'EOF'
Usage: scripts/pack-release.sh --id <id> --target <triple> --bin-dir <dir> [options]

Package the release tarball. Sole writer of the member list consumed by
the curl installer, phux update, and Homebrew.

Options:
  --id <id>           Artifact id (vX.Y.Z or next.<sha>)
  --target <triple>   Rust target triple
  --bin-dir <dir>     Directory containing phux and phux-mcp
  --out-dir <dir>     Output directory (default: .)
  --docs-dir <dir>    Docs to copy (default: repository root)
  --smoke             Run the release-binary smoke checks (needs jq)
  --keep-stage        Leave the staging directory after tarring
  --help              Show this help
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

id=""
target=""
bin_dir=""
out_dir="."
docs_dir=""
smoke=0
keep_stage=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --id)
      [ "$#" -ge 2 ] || die "--id requires a value"
      id="$2"
      shift 2
      ;;
    --target)
      [ "$#" -ge 2 ] || die "--target requires a value"
      target="$2"
      shift 2
      ;;
    --bin-dir)
      [ "$#" -ge 2 ] || die "--bin-dir requires a value"
      bin_dir="$2"
      shift 2
      ;;
    --out-dir)
      [ "$#" -ge 2 ] || die "--out-dir requires a value"
      out_dir="$2"
      shift 2
      ;;
    --docs-dir)
      [ "$#" -ge 2 ] || die "--docs-dir requires a value"
      docs_dir="$2"
      shift 2
      ;;
    --smoke)
      smoke=1
      shift
      ;;
    --keep-stage)
      keep_stage=1
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

[ -n "$id" ] || die "--id is required"
[ -n "$target" ] || die "--target is required"
[ -n "$bin_dir" ] || die "--bin-dir is required"

case "$id" in
  */*|*".."*) die "--id must not contain a path" ;;
esac
case "$target" in
  */*|*".."*) die "--target must not contain a path" ;;
esac

root="$(cd "$(dirname "$0")/.." && pwd)"
docs_dir="${docs_dir:-$root}"
mkdir -p "$out_dir"
out_dir="$(cd "$out_dir" && pwd)"
bin_dir="$(cd "$bin_dir" && pwd)"
docs_dir="$(cd "$docs_dir" && pwd)"

stage="phux-${id}-${target}"
stage_dir="${out_dir}/${stage}"
archive="${out_dir}/${stage}.tar.gz"

rm -rf "$stage_dir"
mkdir -p "$stage_dir"

for bin in "${RELEASE_BINS[@]}"; do
  [ -f "${bin_dir}/${bin}" ] || die "missing binary: ${bin_dir}/${bin}"
  cp -f "${bin_dir}/${bin}" "${stage_dir}/${bin}"
  [ -x "${stage_dir}/${bin}" ] || die "staged ${bin} is not executable"
done

for doc in "${RELEASE_DOCS[@]}"; do
  [ -f "${docs_dir}/${doc}" ] || die "missing release doc: ${docs_dir}/${doc}"
  cp -f "${docs_dir}/${doc}" "${stage_dir}/${doc}"
done

if [ "$smoke" -eq 1 ]; then
  command -v jq >/dev/null 2>&1 || die "--smoke requires jq"
  test -x "${stage_dir}/phux" || die "staged phux is not executable"
  test -x "${stage_dir}/phux-mcp" || die "staged phux-mcp is not executable"
  "${stage_dir}/phux" --version
  "${stage_dir}/phux" --skill=quick >/dev/null
  "${stage_dir}/phux" --capabilities --json | jq -e '.schema_version == 1' >/dev/null
  "${stage_dir}/phux" mcp --skill >/dev/null
  "${stage_dir}/phux" mcp --schema | jq -e 'length > 0' >/dev/null
  "${stage_dir}/phux-mcp" --skill >/dev/null
  "${stage_dir}/phux-mcp" --schema | jq -e 'length > 0' >/dev/null
  "${stage_dir}/phux-mcp" </dev/null
fi

tar -czf "$archive" -C "$out_dir" "$stage"

if command -v sha256sum >/dev/null; then
  sha="$(sha256sum "$archive" | cut -d' ' -f1)"
else
  sha="$(shasum -a 256 "$archive" | cut -d' ' -f1)"
fi
echo "${sha}  ${stage}.tar.gz" > "${archive}.sha256"

if [ "$keep_stage" -eq 0 ]; then
  rm -rf "$stage_dir"
fi

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "artifact=${stage}.tar.gz" >> "$GITHUB_OUTPUT"
fi

echo "packaged ${archive}"
echo "sha256   ${sha}"
