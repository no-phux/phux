#!/usr/bin/env bash
# Release resolution reads only the channel manifest (ADR-0175): one fixed URL,
# no GitHub API, and a refusal for any manifest that disagrees with what the
# installer derives itself.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
SH="$(command -v dash || echo /bin/sh)"
GOLDEN="$ROOT/scripts/fixtures/release-manifest"
mkdir -p "$TMP/bin" "$TMP/manifests"
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
  https://github.com/no-phux/phux/releases/download/channels/*.json) ;;
  *) exit 22 ;;
esac
fixture="$RESOLUTION_MANIFESTS/${url##*/}"
[ -f "$fixture" ] || exit 22
if [ -n "$out" ] && [ "$out" != '-' ]; then
  cp "$fixture" "$out"
else
  cat "$fixture"
fi
EOF
chmod 755 "$TMP/bin/curl"
export RESOLUTION_CALLS="$TMP/calls" RESOLUTION_MANIFESTS="$TMP/manifests"

reset_manifests() {
  cp "$GOLDEN/stable.json" "$GOLDEN/next.json" "$TMP/manifests/"
}

# Mutate one manifest with a Python expression over `m`, then re-render it in
# the generator's exact form so only the mutation differs.
mutate() {
  local channel="$1" expression="$2"
  python3 - "$TMP/manifests/$channel.json" "$expression" <<'PY'
import json, sys
path, expression = sys.argv[1], sys.argv[2]
m = json.load(open(path))
exec(expression)
open(path, "w").write(json.dumps(m, indent=2) + "\n")
PY
}

resolve() {
  PATH="$TMP/bin:$PATH" "$SH" "$ROOT/scripts/$1" --dry-run --os darwin --arch arm64 "${@:2}"
}

expect_resolved() {
  local expected="$1" output
  shift
  : > "$TMP/calls"
  output="$(resolve install.sh "$@")"
  grep -Fxq "$expected" <<<"$output" || { echo "install.sh resolved: $output" >&2; exit 1; }
  if grep -Fq 'api.github.com' "$TMP/calls"; then echo 'resolution called the GitHub API' >&2; exit 1; fi
}

expect_failure() {
  local message="$1"
  shift
  if resolve install.sh "$@" >"$TMP/out" 2>"$TMP/err"; then
    echo "install.sh accepted a release it should refuse: $(cat "$TMP/out")" >&2; exit 1
  fi
  [[ ! -s $TMP/out ]]
  grep -Fq -- "$message" "$TMP/err" || { echo "expected '$message', got: $(cat "$TMP/err")" >&2; exit 1; }
}

base=https://github.com/no-phux/phux/releases/download
reset_manifests

# Stable is one manifest read: the URL is derived, the digest is the manifest's.
expect_resolved "archive_url: $base/v9.8.7/phux-v9.8.7-aarch64-apple-darwin.tar.gz"
[[ $(cat "$TMP/calls") == "$base/channels/stable.json" ]]
expect_resolved "sha256: $(printf '1%.0s' {1..64})"
expect_resolved "channel: stable" --channel latest
output="$(resolve install.sh --os linux --arch x86_64)"
grep -Fxq "sha256: $(printf '2%.0s' {1..64})" <<<"$output"
grep -Fq '/phux-v9.8.7-x86_64-unknown-linux-gnu.tar.gz' <<<"$output"
next="$(printf 'a%.0s' {1..40})"
expect_resolved "archive_url: $base/next/phux-next.$next-aarch64-apple-darwin.tar.gz" --channel next
[[ $(cat "$TMP/calls") == "$base/channels/next.json" ]]
PHUX_CHANNEL=next expect_resolved "channel: next"

# Pins never read the manifest and are verified against their own sidecar.
: > "$TMP/calls"
output="$(FAIL_NETWORK=1 resolve install.sh --version v1.2.3)"
grep -Fxq "sha256_url: $base/v1.2.3/phux-v1.2.3-aarch64-apple-darwin.tar.gz.sha256" <<<"$output"
if grep -Fq 'sha256: ' <<<"$output"; then echo 'pin used a manifest digest' >&2; exit 1; fi
for invalid in '' v1.2.3/evil v1.2.3.4 v1..3 v+1.2.3 v01.2.3 $'v1.2.3\nevil' v1.2.3-alpha.1; do
  if resolve install.sh --version "$invalid" >"$TMP/out" 2>"$TMP/err"; then
    echo "accepted invalid pin $invalid" >&2; exit 1
  fi
done
[[ ! -s $TMP/calls ]]
expect_failure '--version pins a stable tag' --channel next --version v1.2.3

# Every disagreement with the derived artifact, and every malformed document,
# is a refusal rather than a guess.
refuse() {
  local channel="$1" expression="$2" message="$3"
  reset_manifests
  mutate "$channel" "$expression"
  expect_failure "$message" --channel "$channel"
}
refuse stable 'm["products"]["phux"]["artifacts"]["aarch64-apple-darwin"]["url"] = "https://evil.example/phux.tar.gz"' 'artifact URL'
refuse stable 'm["products"]["phux"]["artifacts"]["aarch64-apple-darwin"]["name"] = "phux-v9.8.7-evil.tar.gz"' 'artifact name'
refuse stable 'm["products"]["phux"]["release"] = "next"' 'release'
refuse stable 'm["schema_version"] = 2' 'schema_version'
refuse stable 'm["channel"] = "next"' 'names another channel'
refuse stable 'del m["products"]["phux"]' 'does not publish phux'
refuse stable 'del m["products"]["phux"]["artifacts"]["aarch64-apple-darwin"]' 'no phux artifact for aarch64-apple-darwin'
refuse stable 'm["products"]["phux"]["artifacts"]["aarch64-apple-darwin"]["sha256"] = "F" * 64' 'invalid phux digest'
refuse stable 'm["products"]["phux"]["version"] = "09.8.7"' 'invalid phux version'
refuse next 'm["products"]["phux"]["sha"] = "../evil"' 'invalid phux commit'
refuse next 'm["products"]["phux"]["sha"] = "A" * 40' 'invalid phux commit'
refuse stable 'm["products"]["phux"]["extra"] = [1]' 'malformed'
refuse stable 'm["products"]["phux"]["extra"] = True' 'malformed'
refuse stable 'm["products"]["phux"]["extra"] = {}' 'malformed'
refuse stable 'm["products"]["phux"]["extra"] = "has space"' 'malformed'

# Line-shape damage the generator never writes.
damage() {
  local message="$1" program="$2"
  reset_manifests
  awk "$program" "$GOLDEN/stable.json" > "$TMP/manifests/stable.json"
  expect_failure "$message"
}
damage 'malformed' 'NR == 2 { print "  \"schema_version\": 1," } { print }'
damage 'malformed' '{ sub(/"channel": "stable",/, "\"channel\": \"stable\""); print }'
damage 'malformed' '{ printf "%s\r\n", $0 }'
damage 'malformed' '{ sub(/^  "channel"/, "   \"channel\""); print }'
damage 'malformed' 'NR < 20 { print }'
damage 'malformed' '{ print } END { print "{}" }'
damage 'malformed' '{ gsub(/  /, "\t"); print }'
reset_manifests
head -c 1048577 /dev/zero | tr '\0' ' ' > "$TMP/manifests/stable.json"
expect_failure '1048576'
reset_manifests
rm "$TMP/manifests/stable.json"
expect_failure 'could not download the stable release manifest'
reset_manifests
FAIL_NETWORK=1 expect_failure 'pass --version to pin a release'

# The wget fallback works with a genuinely curl-free PATH and the system awk.
reset_manifests
mkdir "$TMP/wget-bin"
cp "$TMP/bin/curl" "$TMP/wget-bin/wget"
ln -s "$(command -v awk)" "$TMP/wget-bin/awk"
for tool in cat cp grep mktemp rm wc dirname basename; do
  ln -s "$(command -v "$tool")" "$TMP/wget-bin/$tool"
done
PATH="$TMP/wget-bin" "$SH" "$ROOT/scripts/install.sh" --dry-run --os darwin --arch arm64 > "$TMP/wget-out"
grep -Fq "/v9.8.7/phux-v9.8.7-aarch64-apple-darwin.tar.gz" "$TMP/wget-out"

# Every awk this repo's installers meet reads the golden manifests the same way.
for awk in awk mawk gawk original-awk "busybox awk"; do
  program="$(command -v "${awk%% *}")" || continue
  [[ $awk != *' '* ]] || program="$program ${awk#* }"
  for channel in stable next; do
    mkdir -p "$TMP/awk-bin"
    printf '#!/bin/sh\nexec %s "$@"\n' "$program" > "$TMP/awk-bin/awk"
    chmod 755 "$TMP/awk-bin/awk"
    PATH="$TMP/awk-bin:$PATH" "$SH" -c '. "$1"; manifest_read "$2" "*"' parser \
      "$ROOT/scripts/lib/install-release.sh" "$GOLDEN/$channel.json" > "$TMP/flat-$channel"
    [[ $(wc -l < "$TMP/flat-$channel") -eq 24 ]] || { echo "$awk flattened $channel to $(wc -l < "$TMP/flat-$channel") lines" >&2; exit 1; }
    grep -Fxq "products.desktop.minimum_macos=27.0" "$TMP/flat-$channel"
  done
done

[[ -z $(find "$TMPDIR" -mindepth 1 -print -quit) ]]
echo 'installer release resolution tests passed'
