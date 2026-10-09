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
fixture="$RESOLUTION_PAGES/$page.json"
[ -f "$fixture" ] || fixture="$RESOLUTION_PAGES/empty.json"
if [ -n "$out" ] && [ "$out" != '-' ]; then
  cp "$fixture" "$out"
else
  cat "$fixture"
fi
EOF
chmod 755 "$TMP/bin/curl"
export RESOLUTION_CALLS="$TMP/calls" RESOLUTION_PAGES="$TMP/pages"
printf '[]\n' > "$TMP/pages/empty.json"

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
rm "$TMP/pages/2.json"

# Both stable consumers must choose numerically, not by publication order or
# lexical order, and keep searching after the first eligible release.
for script in install.sh install-cockpit.sh; do
  prefix=v
  [[ $script != install-cockpit.sh ]] || prefix=cockpit-v
  for versions in '1.9.99 1.10.0 1.8.999' '1.0.9 1.0.10 1.0.8' \
    '9.99.99 10.0.0 8.999.999' '1.0.9007199254740992 1.0.9007199254740993 1.0.8'; do
    read -r first highest last <<< "$versions"
    printf '[{"tag_name":"%s%s","draft":false,"prerelease":false},{"tag_name":"%s%s","draft":false,"prerelease":false},{"tag_name":"%s%s","draft":false,"prerelease":false}]\n' \
      "$prefix" "$first" "$prefix" "$highest" "$prefix" "$last" > "$TMP/pages/1.json"
    : > "$TMP/calls"
    expect_tag "$script" "$prefix$highest"
    [[ $(wc -l < "$TMP/calls") -eq 2 ]]
  done
  printf '[{"tag_name":"%s1.0.9","draft":false,"prerelease":false}]\n' "$prefix" > "$TMP/pages/1.json"
  printf '[{"tag_name":"%s1.0.8","draft":false,"prerelease":false},{"tag_name":"%s1.0.10","draft":false,"prerelease":false},{"tag_name":"%s99.0.0","draft":true,"prerelease":false},{"tag_name":"%s99.0.0-alpha.1","draft":false,"prerelease":true},{"tag_name":"other-v999.0.0","draft":false,"prerelease":false}]\n' \
    "$prefix" "$prefix" "$prefix" "$prefix" > "$TMP/pages/2.json"
  : > "$TMP/calls"
  expect_tag "$script" "${prefix}1.0.10"
  [[ $(wc -l < "$TMP/calls") -eq 3 ]]
  rm "$TMP/pages/2.json"
done

# A match cannot hide malformed metadata on a later page.
printf '[{"tag_name":"v1.0.10","draft":false,"prerelease":false}]\n' > "$TMP/pages/1.json"
printf '[{"tag_name":"v1.0.11","draft":"false","prerelease":false}]\n' > "$TMP/pages/2.json"
if resolve install.sh > "$TMP/out" 2> "$TMP/err"; then
  echo 'core ignored malformed later page' >&2; exit 1
fi
[[ ! -s $TMP/out ]]
grep -Fq 'invalid release list on page 2' "$TMP/err"
rm "$TMP/pages/2.json"

# Pins are syntax-checked locally; neither valid nor invalid pins query GitHub.
: > "$TMP/calls"
FAIL_NETWORK=1 resolve install.sh --version v9.8.7 >/dev/null
FAIL_NETWORK=1 resolve install-cockpit.sh --version 9.8.7 >/dev/null
for invalid in '' v1.2.3/evil v1.2.3.4 v1..3 v+1.2.3 v01.2.3 $'v1.2.3\nevil'; do
  if resolve install.sh --version "$invalid" >"$TMP/out" 2>"$TMP/err"; then
    echo "core accepted invalid pin $invalid" >&2; exit 1
  fi
done
for invalid in '' 1.2.3.4 1..3 01.2.3; do
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
    '[{"tag_name":"v1.2.3","draft":false,"prerelease":false,"ignored":"\u123"}]' \
    '[{"tag_name":"v1.2.3","draft":false,"prerelease":false,"ignored":"\x"}]' \
    '[{"tag_name":"v1.2.3","draft":false,"prerelease":false,"ignored":1e+}]' \
    '[{"tag_name":"v1.2.3","draft":false,"prerelease":false,"ignored":01}]' \
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
  # A match still scans the bounded window and selects the maximum on page 10.
  prefix=v
  [[ $script != install-cockpit.sh ]] || prefix=cockpit-v
  printf '[{"tag_name":"%s1.0.9","draft":false,"prerelease":false}]\n' "$prefix" > "$TMP/pages/1.json"
  printf '[{"tag_name":"%s1.0.10","draft":false,"prerelease":false}]\n' "$prefix" > "$TMP/pages/10.json"
  printf '[{"tag_name":"%s99.0.0","draft":false,"prerelease":false}]\n' "$prefix" > "$TMP/pages/11.json"
  : > "$TMP/calls"
  expect_tag "$script" "${prefix}1.0.10"
  [[ $(wc -l < "$TMP/calls") -eq 10 ]]
  rm "$TMP/pages/10.json" "$TMP/pages/11.json"
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

# The system awk used to copy the whole unread page for each token: a valid
# 600 KiB ignored array could take tens of seconds. Keep dense numeric/string
# arrays and cross-chunk escaped strings, numbers and keywords in the real
# installer path. Timing is measured separately, not asserted on loaded CI.
for atom in '10' '"x"' '[-12.34e+56,true,false,null,"\u0061\"\\\/\b\f\n\r\t"]'; do
  count=200000
  [[ $atom == '['* ]] && count=2048
  JSON_ATOM="$atom" awk -v count="$count" 'BEGIN {
    printf "[{\"tag_name\":\"v9.8.7\",\"draft\":false,\"prerelease\":false,\"ignored\":["
    for (i = 0; i < count; i++) printf "%s%s", i ? "," : "", ENVIRON["JSON_ATOM"]
    printf "]},{\"tag_name\":\"cockpit-v9.8.7\",\"draft\":false,\"prerelease\":false}]\n"
  }' > "$TMP/pages/1.json"
  for script in install.sh install-cockpit.sh; do
    PATH="$TMP/wget-bin" "$SH" "$ROOT/scripts/$script" --dry-run --os darwin --arch arm64 > "$TMP/dense-out"
    grep -Fq '9.8.7/' "$TMP/dense-out"
  done
done

# A long token must carry across chunks without being truncated, including an
# escape and closing quote near a boundary. Pretty input is coalesced as well.
awk 'BEGIN {
  print "[{\"tag_name\":\"v9.8.7\",\"draft\":false,\"prerelease\":false,\"ignored\":{"
  printf "\"body\":\""
  for (i = 0; i < 4093; i++) printf "x"
  print "\\u0061\\\"tail\",\"array\":["
  for (i = 0; i < 4096; i++) print i ? ",false" : "true"
  print "]}}]"
}' > "$TMP/pages/1.json"
PATH="$TMP/wget-bin" "$SH" "$ROOT/scripts/install.sh" --dry-run --os darwin --arch arm64 > "$TMP/long-out"
grep -Fq 'v9.8.7/' "$TMP/long-out"

# A near-limit ordinary body previously rescanned its growing prefix for each
# 1 KiB chunk. Check full parsing and selection here; benchmark elapsed time
# separately so the regression suite has no CPU-speed-dependent deadline.
awk 'BEGIN {
  printf "[{\"tag_name\":\"v9.8.7\",\"draft\":false,\"prerelease\":false,\"body\":\""
  for (i = 0; i < 1000000; i++) printf "x"
  print "\"},{\"tag_name\":\"cockpit-v9.8.7\",\"draft\":false,\"prerelease\":false}]"
}' > "$TMP/pages/1.json"
for script in install.sh install-cockpit.sh; do
  PATH="$TMP/wget-bin" "$SH" "$ROOT/scripts/$script" --dry-run --os darwin --arch arm64 > "$TMP/near-limit-out"
  grep -Fq '9.8.7/' "$TMP/near-limit-out"
done

# Position each escape and its hex digits on both sides of a chunk boundary.
# Invoke the canonical parser with system awk; generated copies are checked by
# the surface gate. A selected tag must never escape a malformed trailing body.
for offset in {0..7}; do
  for suffix in '\u0061tail"}]' '\"tail"}]' '\\tail"}]' '\/tail"}]' \
    '\b\f\n\r\t"}]' '"}]' '\u123"}]' '\u12g4"}]' '\x"}]' \
    '\' '\u' '\u0' '\u00' '\u000' $'\n"}]'; do
    JSON_SUFFIX="$suffix" awk -v offset="$offset" 'BEGIN {
      prefix = "[{\"tag_name\":\"v9.8.7\",\"draft\":false,\"prerelease\":false,\"body\":\""
      printf "%s", prefix
      for (i = length(prefix); i < 1024 - offset; i++) printf "x"
      printf "%s", ENVIRON["JSON_SUFFIX"]
    }' > "$TMP/boundary.json"
    case "$suffix" in
      '\u0061tail"}]'|'\"tail"}]'|'\\tail"}]'|'\/tail"}]'|'\b\f\n\r\t"}]'|'"}]') expected=0 ;;
      *) expected=1 ;;
    esac
    status=0
    PATH="$TMP/wget-bin" "$SH" -c '. "$1"; release_page v "$2"' parser \
      "$ROOT/scripts/lib/install-release.sh" "$TMP/boundary.json" > "$TMP/boundary-out" || status=$?
    [[ $status == "$expected" ]] || { echo "escape boundary failed: offset=$offset suffix=$suffix" >&2; exit 1; }
    if [[ $expected == 0 ]]; then
      grep -Fxq 'v9.8.7' "$TMP/boundary-out"
    else
      [[ ! -s $TMP/boundary-out ]]
    fi
  done
done
[[ -z $(find "$TMPDIR" -mindepth 1 -print -quit) ]]
echo 'installer release resolution tests passed'
