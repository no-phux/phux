#!/bin/sh
# Served verbatim at https://phux.sh/install-desktop (POSIX sh).
# Installs the checksummed GPUI desktop client. Installs the phux CLI first
# when none is found, but never replaces an existing CLI or stops its server.
# Existing app installation rolls back on failure.
set -eu

# BEGIN shared CLI output
#!/bin/sh
# Human status only. Source locally or embed in standalone installers with
# sync-install-resolver.sh. Keep data, help, and child-tool output untouched.

phux_color_enabled() {
  [ -z "${NO_COLOR:-}" ] || return 1
  [ "${TERM:-}" != dumb ] || return 1
  case "${CLICOLOR_FORCE:-}" in ''|0) ;; *) return 0 ;; esac
  [ "${CLICOLOR:-}" != 0 ] || return 1
  [ -t 2 ]
}

# A subshell keeps rendering variables out of the caller's namespace. printf
# always uses a fixed format: percent signs and backslashes in messages are data.
phux_message() (
  phux_label="$1"
  shift
  if phux_color_enabled; then
    case "$phux_label" in
      error) phux_style=31 ;;
      warn) phux_style=33 ;;
      ok) phux_style=32 ;;
      *) phux_style=36 ;;
    esac
    printf '\033[1mphux\033[0m: \033[%sm%s\033[0m: %s\n' "$phux_style" "$phux_label" "$*" >&2
  else
    printf 'phux: %s: %s\n' "$phux_label" "$*" >&2
  fi
)

phux_error() { phux_message error "$@"; }
phux_warn() { phux_message warn "$@"; }
phux_info() { phux_message info "$@"; }
phux_ok() { phux_message ok "$@"; }
phux_hint() {
  printf '%s\n' "$*" | while IFS= read -r phux_line; do
    printf '  hint: %s\n' "${phux_line#"${phux_line%%[![:space:]]*}"}" >&2
  done
}
# END shared CLI output

usage() {
  cat <<'EOF'
Usage: install-desktop.sh [--channel stable|latest|next] [options]

Installs the latest stable desktop client for Apple-silicon macOS. Installs the
phux CLI first if none is found; never upgrades an existing CLI or stops a
server. Releases are ad-hoc
signed, not Apple-notarized. The installer removes quarantine after
checksum/signature checks.

  --channel <stable|latest|next> Stable by default; latest is a stable alias.
  --version <tag|X.Y.Z>         Pin stable, or next.<40-character SHA> with --channel next.
  --revision <SHA>             Pin immutable next artifact (requires --channel next).
  --sha256 <hex>               Expected digest of a pinned artifact (instead of its sidecar).
  --stage-dir <absolute dir>   Verify and stage Phux.app there; install nothing else.
  --applications-dir <dir>      Default: /Applications if writable, else ~/Applications.
  --bin-dir <dir>               phux-desktop launcher (default: ~/.local/bin).
  --dry-run                    Print release URLs and destination without installing.
  --help                       Show this help.
EOF
}

die() {
  phux_error "$*"
  exit 1
}

# POSIX stand-in for bash's `printf %q`, which dash rejects outright. A word
# built only from characters no shell treats specially prints bare; anything
# else is single-quoted, with an embedded single quote rewritten as the usual
# '\'' dance. Either way the result pastes straight back into a POSIX shell.
shell_quote() {
  case "$1" in
    '' | *[!A-Za-z0-9_./:@%+=-]*)
      printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
      ;;
    *)
      printf '%s' "$1"
      ;;
  esac
}

# Small PATH launcher that opens the installed bundle through Launch Services.
# Extra argv is ignored: `open` would otherwise treat it as more files to open.
write_cli_launcher() {
  app_path="$1"
  [ -n "$bin_dir" ] || return 1
  mkdir -p "$bin_dir" || return 1
  staged="$(mktemp "${bin_dir}/.phux-desktop.XXXXXX")" || return 1
  {
    printf '%s\n' '#!/bin/sh'
    printf '%s\n' '# Installed by the Phux installer. Opens the app bundle'
    printf '%s\n' '# through Launch Services so Dock identity stays this copy.'
    printf 'APP=%s\n' "$(shell_quote "$app_path")"
    printf '%s\n' 'if [ ! -d "$APP" ] || [ ! -f "$APP/Contents/Info.plist" ]; then'
    printf '%s\n' '  echo "phux: error: Phux is not installed at $APP" >&2'
    printf '%s\n' '  echo "  hint: curl -fsSL https://phux.sh/install-desktop | sh" >&2'
    printf '%s\n' '  exit 1'
    printf '%s\n' 'fi'
    printf '%s\n' 'exec open "$APP"'
  } > "$staged" || {
    rm -f "$staged"
    return 1
  }
  chmod 755 "$staged" || {
    rm -f "$staged"
    return 1
  }
  mv -f "$staged" "${bin_dir}/phux-desktop" || {
    rm -f "$staged"
    return 1
  }
}

version=""
revision=""
channel="${PHUX_CHANNEL:-stable}"
applications_dir="${PHUX_DESKTOP_APPLICATIONS_DIR:-}"
applications_dir_flag=""
expected_sha256=""
stage_dir=""
bin_dir="${PHUX_DESKTOP_BIN_DIR:-${PHUX_INSTALL_DIR:-}}"
dry_run=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --channel)
      [ "$#" -ge 2 ] || die "--channel requires a value"
      channel="$2"
      shift 2
      ;;
    --revision)
      [ "$#" -ge 2 ] || die "--revision requires a value"
      revision="$2"
      printf '%s\n' "$revision" | LC_ALL=C grep -Eq '^[0-9a-f]{40}$' \
        || die "--revision must be a 40-character lowercase SHA"
      shift 2
      ;;
    --version)
      [ "$#" -ge 2 ] || die "--version requires a value"
      [ -n "$2" ] || die "--version requires a nonempty release tag"
      version="$2"
      shift 2
      ;;
    --applications-dir)
      [ "$#" -ge 2 ] || die "--applications-dir requires a value"
      applications_dir="$2"
      applications_dir_flag=1
      shift 2
      ;;
    --sha256)
      [ "$#" -ge 2 ] || die "--sha256 requires a value"
      expected_sha256="$2"
      printf '%s\n' "$expected_sha256" | LC_ALL=C grep -Eq '^[0-9a-f]{64}$' \
        || die "--sha256 must be 64 lowercase hex characters"
      shift 2
      ;;
    --stage-dir)
      [ "$#" -ge 2 ] || die "--stage-dir requires a value"
      stage_dir="$2"
      case "$stage_dir" in
        /?*) ;;
        *) die "--stage-dir must be an absolute path" ;;
      esac
      stage_dir="${stage_dir%/}"
      shift 2
      ;;
    --bin-dir)
      [ "$#" -ge 2 ] || die "--bin-dir requires a value"
      bin_dir="$2"
      shift 2
      ;;
    --dry-run)
      dry_run=1
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

# BEGIN shared release resolver
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
    curl -fsSL --connect-timeout 10 --max-time 30 "$manifest_url" -o "$2" \
      || die "could not download the ${manifest_channel} release manifest (${manifest_url}); retry, or pass --version to pin a release"
  elif command -v wget >/dev/null 2>&1; then
    wget -q --timeout=30 --tries=1 -O "$2" "$manifest_url" \
      || die "could not download the ${manifest_channel} release manifest (${manifest_url}); retry, or pass --version to pin a release"
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
# END shared release resolver

[ "$(uname -s)" = Darwin ] || die "Phux desktop is macOS-only"
case "$(uname -m)" in
  arm64|aarch64) ;;
  *) die "Phux desktop ships arm64 only" ;;
esac
os_version="$(/usr/bin/sw_vers -productVersion)"
[ "${os_version%%.*}" -ge 27 ] || die "Phux desktop requires macOS 27 or later"

case "$channel" in
  latest) channel=stable ;;
  stable|next) ;;
  *) die "--channel must be stable, latest, or next" ;;
esac
[ -z "$revision" ] || {
  [ "$channel" = next ] || die "--revision requires --channel next"
  [ -z "$version" ] || die "--revision and --version cannot be combined"
  version="next.$revision"
}
[ -z "$expected_sha256" ] || [ -n "$version" ] \
  || die "--sha256 requires --version or --revision"

# A pin is verified against its own published sidecar, or against --sha256
# when the caller (Phux Desktop) already holds the manifest digest. Everything
# else comes from the channel manifest (ADR-0175): this script derives the
# artifact name and URL itself and refuses a manifest that disagrees.
next_sha=""
semver=""
from_manifest=0
manifest_dir="$(mktemp -d)"
trap 'rm -rf "$manifest_dir"' EXIT
if [ -z "$version" ]; then
  manifest_path="${manifest_dir}/${channel}.json"
  fetch_manifest "$channel" "$manifest_path"
  manifest_product "$manifest_path" desktop aarch64-apple-darwin
  semver="$mf_version"
  if [ "$channel" = next ]; then
    next_sha="$mf_sha"
    version="next.$next_sha"
  else
    version="desktop-v$semver"
  fi
  expected_sha256="$mf_sha256"
  from_manifest=1
fi
if [ "$channel" = next ]; then
  next_sha="${version#next.}"
  printf '%s\n' "$next_sha" | LC_ALL=C grep -Eq '^[0-9a-f]{40}$' \
    && [ "$version" = "next.$next_sha" ] \
    || die "--version for next must be next.<40-character lowercase SHA>"
else
  case "$version" in
    desktop-v*) semver="${version#desktop-v}" ;;
    *) semver="$version"; version="desktop-v$semver" ;;
  esac
  valid_release_tag "$version" desktop-v \
    || die "--version must be desktop-vX.Y.Z (got ${version})"
fi

if [ -n "$stage_dir" ]; then
  # Staging locks and renames on the Applications volume the app swaps into.
  [ -z "$applications_dir_flag" ] || die "--stage-dir and --applications-dir cannot be combined"
  applications_dir="$(dirname "$stage_dir")"
elif [ -z "$applications_dir" ]; then
  if [ -d "/Applications" ] && [ -w "/Applications" ]; then
    applications_dir="/Applications"
  else
    applications_dir="${HOME:-}/Applications"
  fi
fi
[ -n "$applications_dir" ] || die "--applications-dir resolved to an empty path"

if [ -z "$bin_dir" ] && [ -n "${HOME:-}" ]; then
  bin_dir="${HOME}/.local/bin"
fi

base_url="https://github.com/no-phux/phux/releases/download/${version}"
zip_name="phux-desktop-${semver}-macos-arm64.zip"
sums_url="${base_url}/SHA256SUMS"
zip_url="${base_url}/${zip_name}"
if [ "$channel" = next ]; then
  base_url="https://github.com/no-phux/phux/releases/download/next"
  zip_name="phux-desktop-next.${next_sha}-macos-arm64.zip"
  zip_url="${base_url}/${zip_name}"
  sums_url="${zip_url}.sha256"
fi
if [ "$from_manifest" -eq 1 ]; then
  manifest_release="$version"
  [ "$channel" != next ] || manifest_release=next
  manifest_expect release "$manifest_release" "$mf_release"
  manifest_expect "artifact name" "$zip_name" "$mf_name"
  manifest_expect "artifact URL" "$zip_url" "$mf_url"
fi

if [ "$dry_run" -eq 1 ]; then
  echo "tag: ${version}"
  echo "channel: ${channel}"
  echo "zip_url: ${zip_url}"
  echo "sha256_url: ${sums_url}"
  [ -z "$expected_sha256" ] || echo "sha256: ${expected_sha256}"
  echo "applications_dir: ${applications_dir}"
  [ -z "$stage_dir" ] || echo "stage_dir: ${stage_dir}"
  echo "bin_dir: ${bin_dir}"
  exit 0
fi

for tool in unzip ditto xattr codesign lockf; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is required to install Phux"
done

# Abort a stalled transfer instead of hanging: under 1 KiB/s for a minute.
if command -v curl >/dev/null 2>&1; then
  download() {
    curl -fsSL --connect-timeout 10 --speed-limit 1024 --speed-time 60 "$1" -o "$2"
  }
elif command -v wget >/dev/null 2>&1; then
  download() {
    wget -q --timeout=60 --tries=2 -O "$2" "$1"
  }
else
  die "curl or wget is required to download release artifacts"
fi

# Dock/Spotlight cannot rediscover a one-shot PHUX_BIN or interactive PATH.
find_cli() {
  for candidate in "${HOME}/.local/bin/phux" /opt/homebrew/bin/phux /usr/local/bin/phux "${HOME}/.cargo/bin/phux"; do
    [ -x "$candidate" ] && return 0
  done
  return 1
}
if [ -z "$stage_dir" ] && ! find_cli; then
  # The app needs a CLI it can find from the Dock: install the standard one
  # into ~/.local/bin rather than refusing.
  phux_info "phux CLI not found; installing it first"
  cli_installer="$(mktemp)"
  if ! download https://phux.sh/install "$cli_installer" \
    || ! PHUX_INSTALL_DIR="${HOME}/.local/bin" sh "$cli_installer"; then
    rm -f "$cli_installer"
    die "could not install the phux CLI; run: curl -fsSL https://phux.sh/install | sh"
  fi
  rm -f "$cli_installer"
  find_cli || die "the phux CLI is still missing from ~/.local/bin; run: curl -fsSL https://phux.sh/install | sh"
fi

tmp_dir="$(mktemp -d)"
publish_dir=""
stage_tmp=""
lock_dir=""
publish_started=0
publish_complete=0
published_app=0

rollback_publish() {
  [ "$publish_started" -eq 1 ] || return 0
  [ "$publish_complete" -eq 0 ] || return 0
  if [ "$published_app" -eq 1 ]; then
    rm -rf "${applications_dir}/Phux.app"
  fi
  if [ -e "${publish_dir}/backup/Phux.app" ] || [ -L "${publish_dir}/backup/Phux.app" ]; then
    mv -f "${publish_dir}/backup/Phux.app" "${applications_dir}/Phux.app" || {
      phux_error "rollback failed; old app preserved at ${publish_dir}/backup/Phux.app"
      publish_dir=""
      return 1
    }
  fi
}

cleanup() {
  rollback_publish || true
  if [ -n "$publish_dir" ]; then
    rm -rf "$publish_dir"
  fi
  if [ -n "$stage_tmp" ]; then
    rm -rf "$stage_tmp"
  fi
  rm -rf "$tmp_dir" "$manifest_dir"
}
trap_install_signals() {
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
}
trap cleanup EXIT
trap_install_signals

zip_path="${tmp_dir}/${zip_name}"
sums_path="${tmp_dir}/SHA256SUMS"
extract_dir="${tmp_dir}/extract"

download "$zip_url" "$zip_path"
if [ -z "$expected_sha256" ]; then
  download "$sums_url" "$sums_path"
  # Verify only this asset; never accept a checksum for a different archive.
  expected_sha256="$(awk -v name="$zip_name" '$2 == name { print $1; exit }' "$sums_path")"
  printf '%s\n' "$expected_sha256" | LC_ALL=C grep -Eq '^[0-9a-f]{64}$' \
    || die "SHA256SUMS did not cover ${zip_name}"
fi
verify_sha256 "$zip_path" "$expected_sha256"

validate_zip() {
  members_path="${tmp_dir}/zip.members"
  unzip -Z1 "$zip_path" > "$members_path" || die "could not read archive"
  # The shipped bundle has no symlinks; reject links before extraction rather
  # than letting one archive entry redirect another outside the temporary tree.
  unzip -Z -l "$zip_path" | awk '$1 ~ /^l/ { found = 1 } END { exit found }' \
    || die "archive contains symlinks"
  [ -s "$members_path" ] || die "archive was empty"
  while IFS= read -r member; do
    case "$member" in
      ""|/*|../*|*/../*|*/..) die "unsafe archive member path: $member" ;;
    esac
    case "$member" in
      "Phux.app/"*) ;;
      *) die "unexpected archive member: $member" ;;
    esac
  done < "$members_path"
  grep -Fxq "Phux.app/Contents/Info.plist" "$members_path" \
    || die "archive did not contain a Phux.app bundle"
}

mkdir -p "$extract_dir"
validate_zip
unzip -q -o "$zip_path" -d "$extract_dir"
[ -d "${extract_dir}/Phux.app" ] && [ ! -L "${extract_dir}/Phux.app" ] \
  || die "archive did not contain a Phux.app directory"
[ -f "${extract_dir}/Phux.app/Contents/Info.plist" ] \
  || die "archive did not contain a Phux.app bundle"

[ -x "${extract_dir}/Phux.app/Contents/MacOS/phux-desktop" ] \
  || die "archive did not contain the desktop executable"
codesign --verify --deep --strict "${extract_dir}/Phux.app" \
  || die "desktop app signature verification failed"
identity="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "${extract_dir}/Phux.app/Contents/Info.plist")"
[ "$identity" = dev.phux.desktop ] || die "archive is not the Phux desktop app"
bundle_version="$(/usr/libexec/PlistBuddy -c 'Print :PhuxDesktopVersion' "${extract_dir}/Phux.app/Contents/Info.plist")"
if [ -n "$semver" ]; then
  [ "$bundle_version" = "$semver" ] || die "desktop bundle version does not match ${version}"
else
  valid_release_tag "desktop-v$bundle_version" desktop-v || die "invalid desktop bundle version"
fi
if [ "$channel" = next ]; then
  bundle_sha="$(/usr/libexec/PlistBuddy -c 'Print :PhuxBuildSHA' "${extract_dir}/Phux.app/Contents/Info.plist")"
  [ "$bundle_sha" = "$next_sha" ] || die "desktop bundle revision does not match ${version}"
  bundle_channel="$(/usr/libexec/PlistBuddy -c 'Print :PhuxDesktopChannel' "${extract_dir}/Phux.app/Contents/Info.plist")"
  [ "$bundle_channel" = next ] || die "desktop bundle is not a next-channel build"
else
  # Older stable bundles predate channel metadata; an explicit Next marker
  # must never masquerade as a Stable channel switch.
  bundle_channel="$(/usr/libexec/PlistBuddy -c 'Print :PhuxDesktopChannel' "${extract_dir}/Phux.app/Contents/Info.plist" 2>/dev/null || true)"
  [ -z "$bundle_channel" ] || [ "$bundle_channel" = stable ] \
    || die "desktop bundle is not a stable-channel build"
fi

mkdir -p "$applications_dir"
lock_dir="${applications_dir}/.phux-desktop-install.lock"
# The inherited descriptor keeps the kernel lock until this shell exits, even
# through rollback. Never unlink it: all installers must lock the same inode.
# A killed process releases the lock automatically; no stale PID recovery.
exec 9>"$lock_dir"
lockf -s -t 0 9 || die "another Desktop install is already publishing to ${applications_dir}"

if [ -n "$stage_dir" ]; then
  # Stage beside the installed app and touch nothing else; Phux Desktop swaps
  # the staged bundle in only after it has exited (ADR-0175).
  mkdir -p "$stage_dir"
  stage_tmp="$(mktemp -d "${stage_dir}/.staging.XXXXXX")"
  ditto "${extract_dir}/Phux.app" "${stage_tmp}/Phux.app"
  xattr -d com.apple.quarantine "${stage_tmp}/Phux.app" 2>/dev/null || true
  if [ -e "${stage_dir}/Phux.app" ] || [ -L "${stage_dir}/Phux.app" ]; then
    mv "${stage_dir}/Phux.app" "${stage_tmp}/previous.app"
  fi
  mv "${stage_tmp}/Phux.app" "${stage_dir}/Phux.app"
  echo "staged: ${stage_dir}/Phux.app"
  exit 0
fi

publish_dir="$(mktemp -d "${applications_dir}/.phux-desktop-install.XXXXXX")"
mkdir "${publish_dir}/backup"
# Finish copying on the destination filesystem before touching an existing app.
# Only renames happen during publication, so a failed copy leaves it available.
ditto "${extract_dir}/Phux.app" "${publish_dir}/Phux.app"

installed_path="${applications_dir}/Phux.app"
if [ -e "$installed_path" ] && [ ! -d "$installed_path" ]; then
  die "refusing to replace non-directory install destination: ${installed_path}"
fi

if [ -d "$installed_path" ]; then
  identity="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$installed_path/Contents/Info.plist" 2>/dev/null || true)"
  [ "$identity" = dev.phux.desktop ] || die "refusing to replace an unrelated Phux.app"
fi

publish_started=1
if [ -e "$installed_path" ] || [ -L "$installed_path" ]; then
  mv "$installed_path" "${publish_dir}/backup/Phux.app"
fi
# Mark before mv so interruption immediately after the rename still rolls back.
# A failed backup rename never sets this flag: the original remains untouched.
published_app=1
mv "${publish_dir}/Phux.app" "$installed_path"
publish_complete=1

# Releases without Developer ID credentials are ad-hoc signed; clearing the
# quarantine attribute is what lets such a build launch, and is the same step
# the Homebrew cask performs.
xattr -d com.apple.quarantine "$installed_path" 2>/dev/null || true

phux_ok "installed Phux ${version} to ${applications_dir}"
installed_app="$(CDPATH='' cd "$applications_dir" && pwd -P)/Phux.app"
if write_cli_launcher "$installed_app"; then
  launcher_path="$(CDPATH='' cd "$bin_dir" && pwd -P)/phux-desktop"
  echo "launcher: ${launcher_path}"
else
  phux_warn "could not install phux-desktop launcher; open the app directly"
fi
printf 'next: open %s\n' "$(shell_quote "$installed_app")"
