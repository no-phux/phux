#!/bin/sh
# Embedded verbatim in standalone installers by sync-install-resolver.sh.
# Runtime dependencies are POSIX sh/awk/utilities and curl or wget, never jq or
# Python. Release discovery reads one manifest per channel (ADR-0175); nothing
# here lists GitHub releases or calls the rate-limited GitHub REST API.

valid_release_tag() {
  case "$1" in *[!A-Za-z0-9.-]*) return 1 ;; esac
  printf '%s\n' "$1" | LC_ALL=C grep -Eq "^${2}(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$"
}

# Read the rendered manifest form release_manifest.py writes: two-space
# indentation, one member per line, no arrays, SAFE string values, integer
# literals. Any other line fails closed. WANT empty validates only; `*` prints
# every `dotted.path=value`; otherwise the one value is printed or the read
# fails. Nothing prints unless the whole document parsed.
manifest_read() {
  LC_ALL=C awk -v want="${2:-}" '
    function fail(why) { print "release manifest line " NR ": " why > "/dev/stderr"; bad = 1; exit 1 }
    function prefix(    i, p) { p = ""; for (i = 1; i < depth; i++) p = p path[i] "."; return p }
    function member(key, scalar, value,    full) {
      if (need != "member") fail("missing comma before member")
      full = prefix() key
      if (full in seen) fail("duplicate member " full)
      seen[full] = 1
      if (scalar) { values[full] = value; order[++count] = full }
    }
    function after(comma) { need = comma ? "member" : "close" }
    {
      if (done) fail("content after the closing brace")
      match($0, /^ */); indent = RLENGTH; body = substr($0, RLENGTH + 1)
      comma = sub(/,$/, "", body)
    }
    NR == 1 {
      if ($0 != "{") fail("expected an object")
      depth = 1; need = "member"; next
    }
    indent != 2 * depth && body !~ /^}$/ { fail("unexpected indentation") }
    body ~ /^"[A-Za-z0-9_-]+": \{$/ {
      if (comma) fail("comma after an opening brace")
      key = substr(body, 2, length(body) - 5)
      member(key, 0, "")
      path[depth++] = key; need = "member"; next
    }
    body ~ /^"[A-Za-z0-9_-]+": "[A-Za-z0-9._:\/+-]*"$/ || body ~ /^"[A-Za-z0-9_-]+": (0|[1-9][0-9]*)$/ {
      split_at = index(body, "\": ")
      key = substr(body, 2, split_at - 2)
      value = substr(body, split_at + 3)
      if (value ~ /^"/) value = substr(value, 2, length(value) - 2)
      member(key, 1, value)
      after(comma); next
    }
    body == "}" {
      if (need != "close") fail("trailing comma or empty object")
      depth--
      if (indent != 2 * depth) fail("unexpected indentation")
      if (depth == 0) { if (comma) fail("comma after the document"); done = 1; next }
      after(comma); next
    }
    { fail("unrecognized line") }
    END {
      if (bad) exit 1
      if (!done) { print "release manifest is truncated" > "/dev/stderr"; exit 1 }
      if (want == "*") { for (i = 1; i <= count; i++) print order[i] "=" values[order[i]]; exit 0 }
      if (want == "") exit 0
      if (!(want in values)) exit 1
      print values[want]
    }
  ' "$1"
}

manifest_get() {
  manifest_read "$1" "$2" 2>/dev/null
}

# With 512-byte ulimit blocks the file-size limit itself stops an oversized
# download at 1 MiB; name that as the size refusal, not a network failure.
manifest_fetch_failed() {
  if [ -f "$1" ] && [ "$(wc -c < "$1")" -ge 1048576 ]; then
    die "the ${manifest_channel} release manifest exceeds 1048576 bytes"
  fi
  die "could not download the ${manifest_channel} release manifest (${manifest_url}); retry, or pass --version to pin a release"
}

# The manifest URL is a constant; CHANNEL is the closed set stable|next.
fetch_manifest() (
  manifest_channel="$1"
  case "$manifest_channel" in stable|next) ;; *) die "unknown release channel: $manifest_channel" ;; esac
  manifest_url="https://github.com/no-phux/phux/releases/download/channels/${manifest_channel}.json"
  # A file-size limit also bounds downloaders that only enforce a maximum when
  # Content-Length is present (512 or 1024 byte blocks, so at most 2 MiB);
  # the exact 1 MiB limit is checked before awk reads it.
  ulimit -f 2048
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --connect-timeout 10 --max-time 30 "$manifest_url" -o "$2" || manifest_fetch_failed "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -q --timeout=30 --tries=1 -O "$2" "$manifest_url" || manifest_fetch_failed "$2"
  else
    die "curl or wget is required to resolve a release; install one or pass --version"
  fi
  [ "$(wc -c < "$2")" -le 1048576 ] || die "the ${manifest_channel} release manifest exceeds 1048576 bytes"
  manifest_read "$2" || die "the ${manifest_channel} release manifest is malformed"
  [ "$(manifest_get "$2" schema_version)" = 1 ] \
    || die "the ${manifest_channel} release manifest has an unsupported schema_version; update this installer"
  [ "$(manifest_get "$2" channel)" = "$manifest_channel" ] \
    || die "the ${manifest_channel} release manifest names another channel"
)

# Set mf_version, mf_release, mf_sha, mf_name, mf_url, mf_sha256 from one
# product's artifact for TARGET, each checked for shape. Callers still derive
# the expected name, release, and URL themselves and refuse a mismatch.
# shellcheck disable=SC2034 # the mf_* values are read by the embedding installer
manifest_product() {
  mf_key="products.$2"
  mf_version="$(manifest_get "$1" "${mf_key}.version")" \
    || die "the release manifest does not publish $2 on this channel"
  mf_release="$(manifest_get "$1" "${mf_key}.release")" || die "the release manifest names no $2 release"
  mf_sha="$(manifest_get "$1" "${mf_key}.sha")" || die "the release manifest names no $2 commit"
  mf_name="$(manifest_get "$1" "${mf_key}.artifacts.$3.name")" \
    || die "the release manifest publishes no $2 artifact for $3"
  mf_url="$(manifest_get "$1" "${mf_key}.artifacts.$3.url")" || die "the release manifest names no $2 URL for $3"
  mf_sha256="$(manifest_get "$1" "${mf_key}.artifacts.$3.sha256")" || die "the release manifest names no $2 digest for $3"
  printf '%s\n' "$mf_version" | LC_ALL=C grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' \
    || die "the release manifest names an invalid $2 version"
  printf '%s\n' "$mf_sha" | LC_ALL=C grep -Eq '^[0-9a-f]{40}$' || die "the release manifest names an invalid $2 commit"
  printf '%s\n' "$mf_sha256" | LC_ALL=C grep -Eq '^[0-9a-f]{64}$' || die "the release manifest names an invalid $2 digest"
}

# Refuse a manifest field that disagrees with what this installer derived.
manifest_expect() {
  [ "$2" = "$3" ] || die "the release manifest's $1 ($3) does not match the expected $2"
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    die "sha256sum or shasum is required to verify release artifacts"
  fi
}

# The trust gate: nothing is unpacked unless FILE hashes to EXPECTED.
verify_sha256() {
  actual_sha256="$(sha256_of "$1")"
  [ "$actual_sha256" = "$2" ] \
    || die "checksum mismatch for $(basename "$1"): expected $2, downloaded $actual_sha256; nothing was installed"
}
