#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
SH="$(command -v dash || echo /bin/sh)"
mkdir -p "$TMP/bin" "$TMP/pages"
export TMPDIR="$TMP/index-temp"
mkdir "$TMPDIR"
cat > "$TMP/bin/curl" <<'EOF'
#!/bin/sh
set -eu
out=""
url=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o|-O) out="$2"; shift ;;
    https://*) url="$1" ;;
  esac
  shift
done
printf '%s\n' "$url" >> "$RESOLUTION_CALLS"
[ "${FAIL_NETWORK:-0}" = 0 ] || exit 22
case "$url" in
  */latest) printf '%s\n' 'https://github.com/no-phux/phux/releases/tag/cockpit-v99.0.0'; exit ;;
  *'&page='*) page="${url##*&page=}" ;;
  *) page=1 ;;
esac
if [ -n "$out" ] && [ "$out" != '-' ]; then
  cp "$RESOLUTION_PAGES/$page.json" "$out"
else
  cat "$RESOLUTION_PAGES/$page.json"
fi
EOF
chmod 755 "$TMP/bin/curl"
export RESOLUTION_CALLS="$TMP/calls" RESOLUTION_PAGES="$TMP/pages"

resolve() {
  PATH="$TMP/bin:$PATH" "$SH" "$ROOT/scripts/$1" --dry-run --os darwin --arch arm64 "${@:2}"
}

expect_tag() {
  local script="$1" tag="$2" output
  output="$(resolve "$script")"
  grep -Fq "/releases/download/$tag/" <<<"$output" || {
    echo "$script chose the wrong release: $output" >&2; exit 1;
  }
}

cp "$ROOT/scripts/fixtures/install-releases/mixed.json" "$TMP/pages/1.json"
expect_tag install.sh v9.8.7
expect_tag install-cockpit.sh cockpit-v9.8.7

# A stream on a later page must be found, even after a short nonempty page.
printf '[{"tag_name":"other-v1.0.0","draft":false,"prerelease":false}]\n' > "$TMP/pages/1.json"
cp "$ROOT/scripts/fixtures/install-releases/mixed.json" "$TMP/pages/2.json"
: > "$TMP/calls"
expect_tag install.sh v9.8.7
grep -Fq '&page=2' "$TMP/calls"
expect_tag install-cockpit.sh cockpit-v9.8.7

# Pins are syntax-checked locally; neither valid nor invalid pins query GitHub.
: > "$TMP/calls"
FAIL_NETWORK=1 resolve install.sh --version v9.8.7 >/dev/null
FAIL_NETWORK=1 resolve install-cockpit.sh --version 9.8.7 >/dev/null
for invalid in v1.2.3/evil v1.2.3.4 v1..3 v+1.2.3 v01.2.3 $'v1.2.3\nevil'; do
  if resolve install.sh --version "$invalid" >"$TMP/out" 2>"$TMP/err"; then
    echo "core accepted invalid pin $invalid" >&2; exit 1
  fi
done
for invalid in 1.2.3.4 1..3 01.2.3; do
  if resolve install-cockpit.sh --version "$invalid" >"$TMP/out" 2>"$TMP/err"; then
    echo "Cockpit accepted invalid pin $invalid" >&2; exit 1
  fi
done
[[ ! -s $TMP/calls ]]

expect_failure() {
  local script="$1" message="$2"
  if resolve "$script" >"$TMP/out" 2>"$TMP/err"; then
    echo "$script unexpectedly resolved a release" >&2; exit 1
  fi
  [[ ! -s $TMP/out ]]
  grep -Fq -- "$message" "$TMP/err"
}

for script in install.sh install-cockpit.sh; do
  printf '[]\n' > "$TMP/pages/1.json"
  expect_failure "$script" '--version'
  for malformed in 'not json' '{}' '[{"tag_name":"v1.2.3"}]' \
    '[{"tag_name":"v1.2.3","draft":false,"prerelease":false},]' \
    '[{"tag_name":"v1.2.3","draft":false,"prerelease":false}] garbage' \
    '[{"tag_name":"v1.2.3","draft":"false","prerelease":false}]' \
    '[{"tag_name":"v1.2.3","draft":false,"draft":true,"prerelease":false}]'; do
    printf '%s\n' "$malformed" > "$TMP/pages/1.json"
    expect_failure "$script" 'invalid release list'
  done
  for page in {1..10}; do
    printf '[{"tag_name":"other-v1.0.0","draft":false,"prerelease":false}]\n' > "$TMP/pages/$page.json"
  done
  : > "$TMP/calls"
  expect_failure "$script" '10 pages'
  [[ $(wc -l < "$TMP/calls") -eq 10 ]]
  # Size is bounded before parsing even a single huge JSON string/line.
  head -c 1048577 /dev/zero | tr '\0' ' ' > "$TMP/pages/1.json"
  expect_failure "$script" '1048576'
  FAIL_NETWORK=1 expect_failure "$script" 'GitHub access/rate limits'
  {
    printf '[{"tag_name":"v9.8.7","draft":false,"prerelease":false,"body":'
    for _ in {1..130}; do printf '['; done
    printf '0'
    for _ in {1..130}; do printf ']'; done
    printf '}]\n'
  } > "$TMP/pages/1.json"
  expect_failure "$script" 'invalid release list'
done

# Escaped ASCII in a key/tag is JSON-equivalent; nested lookalike fields are
# ignored, and formatting (including CRLF) cannot change release selection.
printf '%s\r\n' '[' '{"tag_\u006eame":"v9.8.\u0037","draft":false,"prerelease":false},' \
  '{"tag_name":"cockpit-v9.8.\u0037","draft":false,"prerelease":false}' ']' > "$TMP/pages/1.json"
expect_tag install.sh v9.8.7
expect_tag install-cockpit.sh cockpit-v9.8.7

# Exercise the fallback using a genuinely curl-free PATH and the system awk,
# rather than a fake curl that merely exits unsuccessfully.
mkdir "$TMP/wget-bin"
cp "$TMP/bin/curl" "$TMP/wget-bin/wget"
ln -s /usr/bin/awk "$TMP/wget-bin/awk"
for tool in cat cp grep mktemp rm wc; do
  ln -s "$(command -v "$tool")" "$TMP/wget-bin/$tool"
done
for script in install.sh install-cockpit.sh; do
  PATH="$TMP/wget-bin" "$SH" "$ROOT/scripts/$script" --dry-run --os darwin --arch arm64 > "$TMP/wget-out"
  grep -Fq '9.8.7/' "$TMP/wget-out"
done
[[ -z $(find "$TMPDIR" -mindepth 1 -print -quit) ]]
echo 'installer release resolution tests passed'
