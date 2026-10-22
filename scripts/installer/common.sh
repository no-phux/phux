#!/bin/sh
# Install phux, and with --desktop Phux Desktop, from a verified release
# (ADR-0176). Served verbatim at https://phux.sh/install. POSIX sh: Debian and
# Ubuntu pipe it to dash, which rejects `set -o pipefail` and `printf %q`.
set -eu

RELEASES=https://github.com/no-phux/phux/releases/download
# minisign keys a release checksum may be signed by (ADR-0178): current
# (9F105950F46456C9), then next (B9E437599627DC6A). One line, kept equal to
# RELEASE_KEYS in crates/phux-client-runtime/src/release/signature.rs by a test.
RELEASE_KEYS='RWTJVmT0UFkQn2Zw1mG2cIcm5O/9kLuffefLpHw3vwhWAxgn/gcsoxiX RWRq3CeWWTfkuYUoONcL0LsC/L3BYav/SIeHRPDMauxHAZnrUd+UoYU1'

phux_message() (
  label="$1"
  shift
  if [ -z "${NO_COLOR:-}" ] && [ "${TERM:-}" != dumb ] && [ -t 2 ]; then
    case "$label" in error) style=31 ;; warn) style=33 ;; ok) style=32 ;; *) style=36 ;; esac
    printf '\033[1mphux\033[0m: \033[%sm%s\033[0m: %s\n' "$style" "$label" "$*" >&2
  else
    printf 'phux: %s: %s\n' "$label" "$*" >&2
  fi
)
die() {
  phux_message error "$*"
  exit 1
}

usage() {
  cat <<'EOF'
Usage: curl -fsSL https://phux.sh/install | sh -s -- [options]

Installs phux and phux-mcp. With --desktop, also installs Phux Desktop
(Apple silicon macOS 27+, or x86_64/arm64 GNU/Linux), installing
the CLI only when none is found.

  --channel <stable|latest|next>  Release channel (default: stable, or $PHUX_CHANNEL).
  --version <X.Y.Z>               Pin a stable release (vX.Y.Z also accepted).
  --revision <sha>                Pin a next build by its 40-character commit.
  --sha256 <hex>                  Expected digest of a pinned artifact, instead of its sidecar.
  --install-dir <dir>             Where phux, phux-mcp and the phux-desktop launcher go
                                  (default: $PHUX_INSTALL_DIR, or ~/.local/bin).
  --no-modify-path                Leave the shell profile alone (also $PHUX_NO_MODIFY_PATH=1).
  --desktop                       Install Phux Desktop.
  --applications-dir <dir>        Desktop destination (default: /Applications if writable,
                                  else ~/Applications on macOS; $XDG_DATA_HOME or
                                  ~/.local/share on Linux; also $PHUX_DESKTOP_APPLICATIONS_DIR).
  --stage-dir <absolute dir>      Verify and stage Phux.app on macOS; install nothing.
  --os <darwin|linux>, --arch <arm64|x86_64>
                                  Override platform detection.
  --dry-run                       Print what would be downloaded and where it would go.
  --help                          Show this help.

With minisign on PATH, the release checksum's signature is verified against
the phux release keys. Set PHUX_REQUIRE_SIGNATURE=1 to refuse to install
without that check (0, the default, falls back to the SHA-256 alone).
EOF
}

# POSIX stand-in for `printf %q`: bare when no shell treats a character
# specially, otherwise single-quoted.
shell_quote() {
  case "$1" in
    '' | *[!A-Za-z0-9_./:@%+=-]*) printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")" ;;
    *) printf '%s' "$1" ;;
  esac
}

# Whether all of VALUE, one line, matches the extended regex PATTERN.
matches() {
  case "$1" in *'
'*) return 1 ;; esac
  printf '%s\n' "$1" | LC_ALL=C grep -Eq "^($2)\$"
}
SEMVER='(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'

# --- downloads and digests ---------------------------------------------------

# A stalled transfer (under 1 KiB/s for a minute) aborts instead of hanging.
download() {
  if command -v curl >/dev/null 2>&1; then
    # Only Desktop's archive transfer opts into a meter. Sidecars and signatures
    # stay silent; curl's unknown-length animation carries no percentage.
    if [ "${3:-}" = archive ] && [ "${PHUX_DESKTOP_PROGRESS:-0}" = 1 ]; then
      curl -fSL --progress-bar --proto =https --connect-timeout 10 --speed-limit 1024 --speed-time 60 "$1" -o "$2"
    else
      curl -fsSL --proto =https --connect-timeout 10 --speed-limit 1024 --speed-time 60 "$1" -o "$2"
    fi
  elif command -v wget >/dev/null 2>&1; then
    wget -q --timeout=60 --tries=2 -O "$2" "$1"
  else
    die "curl or wget is required to download a release"
  fi
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    die "sha256sum or shasum is required to verify a release"
  fi
}

# The trust gate: nothing is unpacked unless FILE hashes to EXPECTED.
verify_sha256() {
  actual="$(sha256_of "$1")"
  [ "$actual" = "$2" ] \
    || die "checksum mismatch for $(basename "$1"): expected $2, downloaded $actual; nothing was installed"
}

# The digest a sha256sum-format SIDECAR publishes for NAME, and only NAME.
sidecar_digest() {
  digest="$(awk -v name="$2" '{ sub(/^\*/, "", $2) } $2 == name { print $1; exit }' "$1")"
  matches "$digest" '[0-9a-f]{64}' || die "$(basename "$1") does not cover $2"
  printf '%s\n' "$digest"
}

# --- the channel manifest ----------------------------------------------------

# Flatten the rendered manifest (two-space indent, one member per line, no
# arrays, restricted strings) into `dotted.path=value` lines. Anything else
# fails closed.
manifest_flat() {
  LC_ALL=C awk '
    function fail(why) { print "release manifest line " NR ": " why > "/dev/stderr"; bad = 1; exit 1 }
    closed { fail("content after the document") }
    NR == 1 { if ($0 != "{") fail("expected an object"); next }
    /^ *"[A-Za-z0-9_-]+": \{$/ {
      match($0, /"[^"]+"/); path[++depth] = substr($0, RSTART + 1, RLENGTH - 2); next
    }
    /^ *"[A-Za-z0-9_-]+": ("[A-Za-z0-9._:\/+-]*"|[0-9]+),?$/ {
      line = $0; sub(/^ */, "", line); sub(/,$/, "", line)
      at = index(line, "\": "); value = substr(line, at + 3); gsub(/"/, "", value)
      prefix = ""; for (i = 1; i <= depth; i++) prefix = prefix path[i] "."
      print prefix substr(line, 2, at - 2) "=" value; next
    }
    /^ *\},?$/ { if (depth == 0) closed = 1; else depth--; next }
    { fail("unrecognized line") }
    END { if (!bad && !closed) { print "release manifest is truncated" > "/dev/stderr"; exit 1 } }
  ' "$1"
}

manifest_value() {
  awk -v key="$2" 'index($0, key "=") == 1 { print substr($0, length(key) + 2); exit }' "$1"
}

# Fetch, flatten and check CHANNEL's manifest into FLAT, once per run.
load_manifest() {
  [ ! -s "$2" ] || return 0
  url="${RELEASES}/channels/$1.json"
  raw="${2}.json"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --proto =https --connect-timeout 10 --max-time 30 --max-filesize 1048576 "$url" -o "$raw" \
      || die "could not download the $1 release manifest ($url); retry, or pin a release"
  else
    download "$url" "$raw" || die "could not download the $1 release manifest ($url)"
  fi
  [ "$(wc -c < "$raw")" -le 1048576 ] || die "the $1 release manifest exceeds 1 MiB"
  manifest_flat "$raw" > "$2" || die "the $1 release manifest is malformed"
  [ "$(manifest_value "$2" schema_version)" = 1 ] \
    || die "the $1 release manifest has an unsupported schema; update this installer"
  [ "$(manifest_value "$2" channel)" = "$1" ] || die "the $1 release manifest names another channel"
}

# Set build_id (version on stable, commit on next) and build_sha256 for
# PRODUCT on TARGET: from PIN and PIN_SHA256 when pinned, otherwise from the
# manifest, whose digest is then the one expected.
resolve() {
  if [ -n "$3" ]; then
    build_id="$3"
    build_sha256="$4"
    return
  fi
  load_manifest "$channel" "${work}/manifest"
  key="products.$1"
  version_found="$(manifest_value "${work}/manifest" "${key}.version")"
  [ -n "$version_found" ] || die "no $1 release is published on the $channel channel yet"
  matches "$version_found" "$SEMVER" || die "the manifest names an invalid $1 version"
  sha_found="$(manifest_value "${work}/manifest" "${key}.sha")"
  matches "$sha_found" '[0-9a-f]{40}' || die "the manifest names an invalid $1 commit"
  build_sha256="$(manifest_value "${work}/manifest" "${key}.artifacts.$2.sha256")"
  matches "$build_sha256" '[0-9a-f]{64}' || die "the manifest publishes no $1 digest for $2"
  if [ "$channel" = next ]; then build_id="$sha_found"; else build_id="$version_found"; fi
}

# The one naming rule (ADR-0176 decision 1). Sets asset_stem, asset_name,
# asset_url and sidecar_url for PRODUCT's build_id on TARGET.
name_asset() {
  case "$2" in
    aarch64-apple-darwin) desktop_platform=macos-arm64 desktop_ext=zip ;;
    x86_64-unknown-linux-gnu) desktop_platform=linux-x64 desktop_ext=tar.gz ;;
    aarch64-unknown-linux-gnu) desktop_platform=linux-arm64 desktop_ext=tar.gz ;;
  esac
  case "$1/$channel" in
    phux/stable) release="v${build_id}" asset_stem="phux-v${build_id}-$2" ext=tar.gz ;;
    phux/next) release=next asset_stem="phux-next.${build_id}-$2" ext=tar.gz ;;
    desktop/stable) release="desktop-v${build_id}" asset_stem="phux-desktop-${build_id}-${desktop_platform}" ext="$desktop_ext" ;;
    desktop/next) release=next asset_stem="phux-desktop-next.${build_id}-${desktop_platform}" ext="$desktop_ext" ;;
  esac
  asset_name="${asset_stem}.${ext}"
  asset_url="${RELEASES}/${release}/${asset_name}"
  sidecar_url="${asset_url}.sha256"
  # Stable desktop releases older than the sidecar carry only SHA256SUMS.
  [ "$1/$channel" != desktop/stable ] || sidecar_url="${RELEASES}/${release}/SHA256SUMS"
}

# Whether a release key signed SIDECAR with SIGNATURE. -H accepts only the
# prehashed signatures CI makes, the same rule phux update enforces.
signed_by_release_key() {
  for key in $RELEASE_KEYS; do
    minisign -V -H -q -P "$key" -m "$1" -x "$2" >/dev/null 2>&1 && return 0
  done
  return 1
}

# With minisign on PATH, the asset's signed sidecar is the trust root: it
# must verify against RELEASE_KEYS and agree with the expected digest, or
# supply it. Without minisign the SHA-256 alone is checked, unless
# PHUX_REQUIRE_SIGNATURE=1 makes that a refusal (ADR-0178 decision 5).
verify_signature() {
  case "${PHUX_REQUIRE_SIGNATURE:-0}" in
    0 | 1) ;;
    *) die "PHUX_REQUIRE_SIGNATURE must be 1 (require a signature) or 0; nothing was installed" ;;
  esac
  if ! command -v minisign >/dev/null 2>&1; then
    [ "${PHUX_REQUIRE_SIGNATURE:-0}" = 0 ] \
      || die "PHUX_REQUIRE_SIGNATURE=1 but minisign is not on PATH; nothing was installed"
    phux_message info "minisign not found; checking the SHA-256 without its release signature"
    return 0
  fi
  download "${asset_url}.sha256" "${work}/signed.sha256" \
    || die "could not download the checksum for ${asset_name}; nothing was installed"
  download "${asset_url}.sha256.minisig" "${work}/signed.sha256.minisig" \
    || die "could not download the signature for ${asset_name}; an unsigned release is never installed"
  signed_by_release_key "${work}/signed.sha256" "${work}/signed.sha256.minisig" \
    || die "the checksum for ${asset_name} is not signed by a phux release key; nothing was installed"
  signed="$(sidecar_digest "${work}/signed.sha256" "$asset_name")"
  if [ -z "$build_sha256" ]; then
    build_sha256="$signed"
  elif [ "$signed" != "$build_sha256" ]; then
    die "the signed checksum for ${asset_name} is ${signed}, not ${build_sha256}; nothing was installed"
  fi
}

# Download PRODUCT's asset for TARGET into the work directory and verify it.
fetch_verified() {
  download "$asset_url" "${work}/${asset_name}" archive \
    || die "could not download ${asset_name}; nothing was installed"
  if [ "${PHUX_DESKTOP_PROGRESS:-0}" = 1 ]; then
    printf '\nphux-update: verifying\n' >&2
  fi
  verify_signature
  if [ -z "$build_sha256" ]; then
    download "$sidecar_url" "${work}/sidecar" \
      || die "could not download the checksum for ${asset_name}; nothing was installed"
    build_sha256="$(sidecar_digest "${work}/sidecar" "$asset_name")"
  fi
  verify_sha256 "${work}/${asset_name}" "$build_sha256"
}

print_plan() {
  printf 'product: %s\nchannel: %s\nurl: %s\n' "$1" "$channel" "$asset_url"
  if [ -n "$build_sha256" ]; then echo "sha256: ${build_sha256}"; else echo "sidecar: ${sidecar_url}"; fi
  echo "destination: $2"
}

trap_signals() {
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
}
