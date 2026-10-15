#!/usr/bin/env bash
# A refused next-tag retarget must not skip the channel pointer.
#
# GITHUB_TOKEN gets HTTP 403 from both the tag ref update and
# `gh release edit --target` when the commit's workflow tree differs from
# the default branch. The archives are already uploaded by then; the pointer
# still has to land.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

SHA="0123456789abcdef0123456789abcdef01234567"
DIST="$TMP/dist"
mkdir -p "$DIST"
for name in \
  "phux-next.${SHA}-aarch64-apple-darwin.tar.gz" \
  "phux-next.${SHA}-x86_64-unknown-linux-gnu.tar.gz" \
  "phux-next.${SHA}-aarch64-unknown-linux-gnu.tar.gz"
do
  printf 'asset %s\n' "$name" > "$DIST/$name"
  printf 'deadbeef  %s\n' "$name" > "$DIST/$name.sha256"
done

cat > "$TMP/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$GH_LOG"
if [[ "$1" == "release" && "$2" == "view" && "$*" == *"--json assets"* ]]; then
  if [[ -f "$GH_ASSETS" ]]; then
    cat "$GH_ASSETS"
  fi
  exit 0
fi
if [[ "$1" == "release" && "$2" == "view" ]]; then
  exit 0
fi
if [[ "$1" == "release" && "$2" == "download" ]]; then
  exit 1
fi
if [[ "$1" == "release" && "$2" == "upload" ]]; then
  for arg in "$@"; do
    case "$arg" in
      --clobber|next) ;;
      *)
        basename "$arg" >> "$GH_ASSETS"
        ;;
    esac
  done
  exit 0
fi
if [[ "$1" == "api" || "$1 $2" == "release edit" ]]; then
  echo "gh: Resource not accessible by integration (HTTP 403)" >&2
  exit 1
fi
echo "unexpected gh invocation: $*" >&2
exit 2
EOF
chmod 755 "$TMP/gh"

export GH_LOG="$TMP/gh.log"
export GH_ASSETS="$TMP/assets"
export GH_REPO="no-phux/phux"
: > "$GH_LOG"
: > "$GH_ASSETS"

out="$TMP/out"
set +e
( cd "$ROOT" && PATH="$TMP:$PATH" bash scripts/publish-next-channel.sh "$SHA" "$DIST" >"$out" 2>"$TMP/err" )
status=$?
set -e

if [[ "$status" -ne 0 ]]; then
  echo "publish script exited $status" >&2
  cat "$TMP/err" >&2
  cat "$out" >&2
  exit 1
fi
if ! grep -Fq "could not retarget the next release to ${SHA}" "$out"; then
  echo "missing retarget warning" >&2
  cat "$out" >&2
  exit 1
fi
if ! grep -qx 'channel.json' "$GH_ASSETS"; then
  echo "channel.json was not uploaded" >&2
  cat "$GH_ASSETS" >&2
  exit 1
fi
if ! grep -Fq "release edit next" "$GH_LOG"; then
  echo "release edit was not attempted" >&2
  cat "$GH_LOG" >&2
  exit 1
fi

echo "next-channel retarget refusal still publishes channel.json"
