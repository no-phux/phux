#!/usr/bin/env bash
# scripts/install.sh for the CLI, through dash where the host has it (Debian
# and Ubuntu pipe it to dash; macOS /bin/sh forgives bashisms): release
# resolution from the channel manifest, the install transaction, PATH
# handling, and the install-desktop shim. The network is a fake curl.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
SH="$(command -v dash || echo /bin/sh)"
GOLDEN="$ROOT/scripts/fixtures/release-manifest"
BASE=https://github.com/no-phux/phux/releases/download
TARGET=x86_64-unknown-linux-gnu
export HOME="$TMP/home" SHELL=/bin/zsh
unset ZDOTDIR XDG_CONFIG_HOME PHUX_NO_MODIFY_PATH PHUX_CHANNEL PHUX_INSTALL_DIR
mkdir -p "$HOME" "$TMP/bin" "$TMP/served"
export SERVED="$TMP/served" CALLS="$TMP/calls"

# Serves channels/*.json and release assets from $SERVED by basename.
cat > "$TMP/bin/curl" <<'EOF'
#!/bin/sh
out="" url=""
while [ "$#" -gt 0 ]; do
  case "$1" in -o | -O) out="$2"; shift ;; https://*) url="$1" ;; esac
  shift
done
printf '%s\n' "$url" >> "$CALLS"
[ "${FAIL_NETWORK:-0}" = 0 ] || exit 22
[ -f "$SERVED/${url##*/}" ] || exit 22
if [ -n "$out" ] && [ "$out" != - ]; then cp "$SERVED/${url##*/}" "$out"; else cat "$SERVED/${url##*/}"; fi
EOF
chmod 755 "$TMP/bin/curl"

fail() { echo "FAIL: $*" >&2; exit 1; }
installer() { PATH="$TMP/bin:${TEST_PATH:-/usr/bin:/bin}" "$SH" "$ROOT/scripts/install.sh" "$@"; }
serve_golden() { cp "$GOLDEN/stable.json" "$GOLDEN/next.json" "$SERVED/"; }
# Rewrite a served manifest with a Python statement over `m`.
mutate() {
  python3 - "$SERVED/$1.json" "$2" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
exec(sys.argv[2])
open(sys.argv[1], "w").write(json.dumps(m, indent=2) + "\n")
PY
}
refused() {
  local message="$1"
  shift
  if installer "$@" >"$TMP/out" 2>"$TMP/err"; then fail "accepted: $* -> $(cat "$TMP/out")"; fi
  [[ -z $message ]] || grep -Fq -- "$message" "$TMP/err" || fail "expected '$message', got: $(cat "$TMP/err")"
}
plan() { installer --dry-run --os darwin --arch arm64 "$@"; }

# --- resolution ----------------------------------------------------------------

serve_golden
: > "$CALLS"
out="$(plan)"
grep -Fxq "url: $BASE/v9.8.7/phux-v9.8.7-aarch64-apple-darwin.tar.gz" <<<"$out" || fail "stable url: $out"
grep -Fxq "sha256: $(printf '1%.0s' {1..64})" <<<"$out" || fail "stable digest: $out"
[[ $(cat "$CALLS") == "$BASE/channels/stable.json" ]] || fail "stable is one manifest read"
grep -Fxq 'channel: stable' <<<"$(plan --channel latest)" || fail "latest is stable"
grep -Fxq "sha256: $(printf '2%.0s' {1..64})" <<<"$(installer --dry-run --os linux --arch x86_64)" || fail "linux digest"
sha_a="$(printf 'a%.0s' {1..40})"
grep -Fxq "url: $BASE/next/phux-next.$sha_a-aarch64-apple-darwin.tar.gz" <<<"$(plan --channel next)" || fail "next url"
grep -Fxq 'channel: next' <<<"$(PHUX_CHANNEL=next plan)" || fail "PHUX_CHANNEL"

# A pin reads no manifest and is verified against its sidecar.
for pin in 1.2.3 v1.2.3; do
  out="$(FAIL_NETWORK=1 plan --version "$pin")"
  grep -Fxq "sidecar: $BASE/v1.2.3/phux-v1.2.3-aarch64-apple-darwin.tar.gz.sha256" <<<"$out" || fail "pin: $out"
done
grep -Fxq "url: $BASE/next/phux-next.$sha_a-aarch64-apple-darwin.tar.gz" \
  <<<"$(FAIL_NETWORK=1 plan --channel next --revision "$sha_a")" || fail "revision pin"
for bad in v1.2.3/evil 1.2.3.4 01.2.3 1.2.3-alpha.1 $'1.2.3\nx'; do
  refused 'pin a release with --version' --dry-run --os darwin --arch arm64 --version "$bad"
done
refused 'pin a build with --revision' --dry-run --channel next --version 1.2.3
refused 'pin a release with --version' --dry-run --revision "$sha_a"
refused '--sha256 needs' --dry-run --sha256 "$(printf 'd%.0s' {1..64})"
refused 'no phux release for darwin/x86_64' --dry-run --os darwin --arch x86_64

# A manifest that cannot be trusted is refused, not interpreted.
refuse_manifest() {
  serve_golden
  mutate "$1" "$2"
  refused "$3" --dry-run --os darwin --arch arm64 --channel "$1"
}
refuse_manifest stable 'm["schema_version"] = 2' 'unsupported schema'
refuse_manifest stable 'm["channel"] = "next"' 'names another channel'
refuse_manifest stable 'del m["products"]["phux"]' 'no phux release is published'
refuse_manifest stable 'del m["products"]["phux"]["sha256"]["aarch64-apple-darwin"]' 'no phux digest for aarch64-apple-darwin'
refuse_manifest stable 'm["products"]["phux"]["sha256"]["aarch64-apple-darwin"] = "F" * 64' 'no phux digest'
refuse_manifest stable 'm["products"]["phux"]["version"] = "09.8.7"' 'invalid phux version'
refuse_manifest next 'm["products"]["phux"]["sha"] = "A" * 40' 'invalid phux commit'
for value in '[1]' 'True' '{}' '"has space"'; do
  refuse_manifest stable "m['products']['phux']['extra'] = $value" 'malformed'
done
serve_golden
head -n 12 "$GOLDEN/stable.json" > "$SERVED/stable.json"
refused 'malformed' --dry-run --os darwin --arch arm64
{ cat "$GOLDEN/stable.json"; echo '{}'; } > "$SERVED/stable.json"
refused 'malformed' --dry-run --os darwin --arch arm64
head -c 1048577 /dev/zero | tr '\0' ' ' > "$SERVED/stable.json"
refused 'exceeds 1 MiB' --dry-run --os darwin --arch arm64
rm "$SERVED/stable.json"
refused 'could not download the stable release manifest' --dry-run --os darwin --arch arm64

# Without curl, wget fetches the manifest; every awk the installer meets
# flattens it the same way.
serve_golden
mkdir "$TMP/wget-bin"
cp "$TMP/bin/curl" "$TMP/wget-bin/wget"
for tool in awk cat cp grep mktemp rm rmdir wc dirname basename sed; do
  ln -s "$(command -v "$tool")" "$TMP/wget-bin/$tool"
done
out="$(PATH="$TMP/wget-bin" "$SH" "$ROOT/scripts/install.sh" --dry-run --os darwin --arch arm64)"
grep -Fq '/v9.8.7/phux-v9.8.7-aarch64-apple-darwin.tar.gz' <<<"$out" || fail "wget: $out"
for awk in mawk gawk original-awk busybox; do
  program="$(command -v "$awk")" || continue
  mkdir -p "$TMP/awk-$awk"
  if [[ $awk == busybox ]]; then
    printf '#!/bin/sh\nexec %s awk "$@"\n' "$program" > "$TMP/awk-$awk/awk"
    chmod 755 "$TMP/awk-$awk/awk"
  else
    ln -sf "$program" "$TMP/awk-$awk/awk"
  fi
  grep -Fxq "sha256: $(printf '1%.0s' {1..64})" <<<"$(TEST_PATH="$TMP/awk-$awk:/usr/bin:/bin" plan)" || fail "$awk"
done
if installer --help | grep -Fq '\'; then fail "--help prints an escape"; fi

# --- installing ----------------------------------------------------------------

package() { # ID: pack a release whose binaries say ID, and serve it
  mkdir -p "$TMP/build-$1"
  for binary in phux phux-mcp; do printf '%s %s\n' "$binary" "$1" > "$TMP/build-$1/$binary"; done
  chmod 755 "$TMP/build-$1/"*
  bash "$ROOT/scripts/pack-release.sh" --id "$1" --target "$TARGET" --bin-dir "$TMP/build-$1" --out-dir "$SERVED" >/dev/null 2>&1
  awk '{ print $1 }' "$SERVED/phux-$1-$TARGET.tar.gz.sha256"
}
digest_stable="$(package v9.8.7)"
digest_next="$(package "next.$sha_a")"
serve_golden
mutate stable "m['products']['phux']['sha256']['$TARGET'] = '$digest_stable'"
mutate next "m['products']['phux']['sha256']['$TARGET'] = '$digest_next'"
install_to() { local dir="$1"; shift; installer --os linux --arch x86_64 --install-dir "$dir" "$@"; }

: > "$CALLS"
install_to "$TMP/stable" 2>"$TMP/err" >/dev/null
grep -Fq 'installed phux 9.8.7' "$TMP/err" || fail "stable install: $(cat "$TMP/err")"
grep -Fxq 'phux v9.8.7' "$TMP/stable/phux" && grep -Fxq stable "$TMP/stable/.phux-channel" || fail "stable files"
if grep -Fq .sha256 "$CALLS"; then fail "a manifest install fetched a sidecar"; fi

install_to "$TMP/next" --channel next >/dev/null 2>&1
grep -Fxq "phux next.$sha_a" "$TMP/next/phux" && grep -Fxq next "$TMP/next/.phux-channel" || fail "next install"

: > "$CALLS"
FAIL_MANIFEST=1 install_to "$TMP/pinned" --version 9.8.7 >/dev/null 2>&1
grep -Fq "phux-v9.8.7-$TARGET.tar.gz.sha256" "$CALLS" || fail "a pin must verify its sidecar"

# A digest the archive does not match installs nothing.
mutate stable "m['products']['phux']['sha256']['$TARGET'] = '0' * 64"
printf 'old phux\n' > "$TMP/stable/phux"
refused 'checksum mismatch' --os linux --arch x86_64 --install-dir "$TMP/stable"
grep -Fxq 'old phux' "$TMP/stable/phux" || fail "a mismatch replaced the binary"
mutate stable "m['products']['phux']['sha256']['$TARGET'] = '$digest_stable'"

no_leftovers() {
  if find "$1" -maxdepth 1 -name '.phux-install*' -print -quit | grep -q .; then fail "$2 left transaction state"; fi
}
no_leftovers "$TMP/stable" "a refused install"

# A failed or interrupted publish restores the previous pair, never half.
cat > "$TMP/bin/mv" <<'EOF'
#!/bin/sh
for last; do :; done
case "$last" in
  "${FAIL_DIR:-/none}/phux-mcp") [ -e "$FAIL_DIR.failed" ] || { : > "$FAIL_DIR.failed"; exit 1; } ;;
  "${SIGNAL_DIR:-/none}/phux") /bin/mv "$@"; kill -"$SIGNAL" "$PPID"; exit 0 ;;
esac
exec /bin/mv "$@"
EOF
chmod 755 "$TMP/bin/mv"
mkdir "$TMP/rollback"
printf 'old phux\n' > "$TMP/rollback/phux"
printf 'old phux-mcp\n' > "$TMP/rollback/phux-mcp"
FAIL_DIR="$TMP/rollback" refused '' --os linux --arch x86_64 --install-dir "$TMP/rollback"
grep -Fxq 'old phux' "$TMP/rollback/phux" && grep -Fxq 'old phux-mcp' "$TMP/rollback/phux-mcp" || fail "rollback"
no_leftovers "$TMP/rollback" "a rolled-back install"
for signal in TERM HUP; do
  dir="$TMP/signal-$signal"
  mkdir "$dir"
  SIGNAL_DIR="$dir" SIGNAL="$signal" refused '' --os linux --arch x86_64 --install-dir "$dir"
  [[ ! -e $dir/phux && ! -e $dir/phux-mcp ]] || fail "$signal left a partial pair"
  no_leftovers "$dir" "a $signal interruption"
done
rm "$TMP/bin/mv"

# A concurrent install is refused, and its lock is left alone.
mkdir -p "$TMP/locked/.phux-install.lock"
refused 'if none is running, remove' --os linux --arch x86_64 --install-dir "$TMP/locked"
[[ -d $TMP/locked/.phux-install.lock ]] || fail "a refused install removed another's lock"

# PATH: no remedy when phux is found, a remedy when shadowed, and one
# idempotent profile line when the directory is off PATH.
rm -rf "$HOME" && mkdir "$HOME"
mkdir -p "$TMP/on-path" "$TMP/shadow"
on="$(cd "$TMP/on-path" && pwd -P)"
out="$(TEST_PATH="$on:/usr/bin:/bin" install_to "$on" 2>/dev/null)"
grep -Fxq 'next: phux' <<<"$out" && ! grep -Fq 'PATH remedy' <<<"$out" || fail "on PATH: $out"
printf '#!/bin/sh\n' > "$TMP/shadow/phux"
chmod 755 "$TMP/shadow/phux"
out="$(TEST_PATH="$TMP/shadow:$on:/usr/bin:/bin" install_to "$on" 2>/dev/null)"
grep -Fq "next: $on/phux" <<<"$out" && grep -Fq 'PATH remedy' <<<"$out" || fail "shadowed: $out"
[[ ! -e $HOME/.zshrc ]] || fail "an on-PATH install edited the profile"
mkdir -p "$TMP/off-path"
off="$(cd "$TMP/off-path" && pwd -P)"
install_to "$off" >/dev/null 2>&1
install_to "$off" >/dev/null 2>&1
[[ $(grep -cFx "export PATH=$off:\"\$PATH\"" "$HOME/.zshrc") -eq 1 ]] || fail "zsh profile line"
SHELL=/usr/bin/fish install_to "$off" >/dev/null 2>&1
grep -Fxq "fish_add_path -g $off" "$HOME/.config/fish/conf.d/phux.fish" || fail "fish profile line"
rm -rf "$HOME/.zshrc"
PHUX_NO_MODIFY_PATH=1 install_to "$off" >/dev/null 2>&1
install_to "$off" --no-modify-path >/dev/null 2>&1
[[ ! -e $HOME/.zshrc ]] || fail "an opted-out install edited the profile"

# The website's install-desktop shim runs the installer with --desktop.
printf '#!/bin/sh\necho "args: $*"\n' > "$SERVED/install"
out="$(PATH="$TMP/bin:/usr/bin:/bin" "$SH" "$ROOT/scripts/install-desktop.sh" --channel next)"
[[ $out == 'args: --desktop --channel next' ]] || fail "shim: $out"
rm "$SERVED/install"
if PATH="$TMP/bin:/usr/bin:/bin" "$SH" "$ROOT/scripts/install-desktop.sh" 2>"$TMP/err"; then fail "shim ran without an installer"; fi
grep -Fq 'could not download' "$TMP/err" || fail "shim failure message"

echo "installer tests passed"
