#!/bin/sh
#
# POSIX sh on purpose. This script is served verbatim at https://phux.sh/install
# and has to survive being piped to `sh`, which is dash on Debian and Ubuntu.
# Two bashisms in particular are fatal there and must not come back:
# `set -o pipefail` (rejected by dash before 0.5.12) and `printf %q` (an
# invalid directive in every dash). `just shellcheck` lints this file as sh
# because of the shebang above, so a new bashism fails the gate rather than a
# stranger's install.
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
Usage: scripts/install.sh [--version <vX.Y.Z>] [--channel stable|next] [options]

Options:
  --version <vX.Y.Z>       Pin a release tag (default: the channel manifest's release).
  --channel <stable|latest|next>
                           Release channel (default: stable, or \$PHUX_CHANNEL).
                           latest is stable; next tracks green main; omit --version with it.
  --install-dir <dir>      Directory for phux and phux-mcp (default: $HOME/.local/bin).
  --no-modify-path         Do not add the install directory to your shell profile
                           (also $PHUX_NO_MODIFY_PATH=1).
  --os <darwin|linux>      Override OS detection.
  --arch <arm64|aarch64|x86_64|amd64>
                           Override architecture detection.
  --dry-run                Print resolved target, URLs, and install dir only.
  --help                   Show this help.
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

version=""
channel=""
install_dir="${PHUX_INSTALL_DIR:-${HOME:-}/.local/bin}"
os=""
arch=""
dry_run=0
modify_path=1
case "${PHUX_NO_MODIFY_PATH:-}" in ''|0) ;; *) modify_path=0 ;; esac

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      [ "$#" -ge 2 ] || die "--version requires a value"
      [ -n "$2" ] || die "--version requires a nonempty release tag"
      version="$2"
      shift 2
      ;;
    --channel)
      [ "$#" -ge 2 ] || die "--channel requires a value"
      channel="$2"
      shift 2
      ;;
    --install-dir)
      [ "$#" -ge 2 ] || die "--install-dir requires a value"
      install_dir="$2"
      shift 2
      ;;
    --os)
      [ "$#" -ge 2 ] || die "--os requires a value"
      os="$2"
      shift 2
      ;;
    --arch)
      [ "$#" -ge 2 ] || die "--arch requires a value"
      arch="$2"
      shift 2
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    --no-modify-path)
      modify_path=0
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
# END shared release resolver
if [ -z "$channel" ]; then
  channel="${PHUX_CHANNEL:-stable}"
fi
case "$channel" in
  latest) channel="stable" ;;
  stable|next) ;;
  *) die "--channel must be stable, latest, or next" ;;
esac

if [ "$channel" = "next" ] && [ -n "$version" ]; then
  die "--version pins a stable tag; omit it when using --channel next"
fi
if [ -n "$version" ]; then
  valid_release_tag "$version" v \
    || die "--version must be a release like vX.Y.Z (got ${version})"
fi

if [ -z "$install_dir" ]; then
  die "--install-dir resolved to an empty path"
fi

if [ -z "$os" ]; then
  case "$(uname -s)" in
    Darwin) os="darwin" ;;
    Linux) os="linux" ;;
    *) die "unsupported OS: $(uname -s)" ;;
  esac
fi

if [ -z "$arch" ]; then
  arch="$(uname -m)"
fi

case "$os" in
  darwin|linux) ;;
  *) die "unsupported OS: $os" ;;
esac

case "$arch" in
  arm64|aarch64|x86_64|amd64) ;;
  *) die "unsupported architecture: $arch" ;;
esac

case "${os}/${arch}" in
  darwin/arm64|darwin/aarch64)
    target="aarch64-apple-darwin"
    ;;
  darwin/x86_64|darwin/amd64)
    die "macOS x86_64 has no official release artifact; use a source build"
    ;;
  linux/x86_64|linux/amd64)
    target="x86_64-unknown-linux-gnu"
    ;;
  linux/arm64|linux/aarch64)
    target="aarch64-unknown-linux-gnu"
    ;;
  *)
    die "unsupported OS/architecture combination: ${os}/${arch}"
    ;;
esac

if [ "$version" = "v0.0.1" ]; then
  if [ "$target" = "x86_64-unknown-linux-gnu" ]; then
    die "v0.0.1's Linux tarball is Nix-linked and not portable; use a newer release or build from source"
  fi
  die "v0.0.1 has no ${target} tarball; use a newer release or build from source"
fi

# A pin is verified against its own published sidecar. Everything else comes
# from the channel manifest (ADR-0175), whose digest is the expected one; the
# installer derives the artifact name and URL itself and refuses a manifest
# that disagrees, so no network string steers the download.
expected_sha256=""
manifest_dir="$(mktemp -d)"
trap 'rm -rf "$manifest_dir"' EXIT
if [ -n "$version" ]; then
  release_tag="$version"
  artifact_id="$version"
else
  manifest_path="${manifest_dir}/${channel}.json"
  fetch_manifest "$channel" "$manifest_path"
  manifest_product "$manifest_path" phux "$target"
  if [ "$channel" = "next" ]; then
    release_tag="next"
    artifact_id="next.${mf_sha}"
    version="next.${mf_sha}"
  else
    version="v${mf_version}"
    release_tag="$version"
    artifact_id="$version"
  fi
  manifest_expect release "$release_tag" "$mf_release"
  expected_sha256="$mf_sha256"
fi

base_url="https://github.com/no-phux/phux/releases/download/${release_tag}"
artifact="phux-${artifact_id}-${target}.tar.gz"
archive_url="${base_url}/${artifact}"
sha_url="${archive_url}.sha256"
if [ -n "$expected_sha256" ]; then
  manifest_expect "artifact name" "$artifact" "$mf_name"
  manifest_expect "artifact URL" "$archive_url" "$mf_url"
fi

if [ "$dry_run" -eq 1 ]; then
  echo "channel: ${channel}"
  echo "target: ${target}"
  echo "archive_url: ${archive_url}"
  echo "sha256_url: ${sha_url}"
  [ -z "$expected_sha256" ] || echo "sha256: ${expected_sha256}"
  echo "install_dir: ${install_dir}"
  exit 0
fi

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

tmp_dir="$(mktemp -d)"
publish_dir=""
lock_dir=""
lock_pending=""
lock_acquired=0
publish_started=0
publish_complete=0
published_phux=0
published_phux_mcp=0

rollback_publish() {
  [ "$publish_started" -eq 1 ] || return 0
  [ "$publish_complete" -eq 0 ] || return 0

  if [ "$published_phux" -eq 1 ]; then
    rm -f "${install_dir}/phux"
  fi
  if [ "$published_phux_mcp" -eq 1 ]; then
    rm -f "${install_dir}/phux-mcp"
  fi
  if [ -e "${publish_dir}/backup/phux" ] || [ -L "${publish_dir}/backup/phux" ]; then
    mv -f "${publish_dir}/backup/phux" "${install_dir}/phux"
  fi
  if [ -e "${publish_dir}/backup/phux-mcp" ] || [ -L "${publish_dir}/backup/phux-mcp" ]; then
    mv -f "${publish_dir}/backup/phux-mcp" "${install_dir}/phux-mcp"
  fi
}

release_install_lock() {
  if [ -n "$lock_pending" ]; then
    rm -rf "$lock_pending" 2>/dev/null || true
    lock_pending=""
  fi
  [ -n "$lock_dir" ] && [ -d "$lock_dir" ] || return 0
  lock_pid=""
  if [ -f "${lock_dir}/pid" ]; then
    lock_pid="$(cat "${lock_dir}/pid" 2>/dev/null || true)"
  fi
  # Drop a lock we created, including a signal between mkdir and lock_acquired=1.
  # A lock with no pid, or another installer's pid, is left untouched.
  if [ "$lock_acquired" -eq 1 ] || [ "$lock_pid" = "$$" ]; then
    rm -f "${lock_dir}/pid"
    rmdir "$lock_dir" 2>/dev/null || true
  fi
}

cleanup() {
  rollback_publish
  if [ -n "$publish_dir" ]; then
    rm -rf "$publish_dir"
  fi
  release_install_lock
  rm -rf "$tmp_dir" "$manifest_dir"
}
trap_install_signals() {
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
}
trap cleanup EXIT
trap_install_signals

archive_path="${tmp_dir}/${artifact}"
sha_path="${archive_path}.sha256"
extract_dir="${tmp_dir}/extract"
stage_dir="${extract_dir}/phux-${artifact_id}-${target}"
stage_name="phux-${artifact_id}-${target}"

download "$archive_url" "$archive_path"
if [ -n "$expected_sha256" ]; then
  verify_sha256 "$archive_path" "$expected_sha256"
else
  download "$sha_url" "$sha_path"
  (
    cd "$tmp_dir"
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum -c "$(basename "$sha_path")"
    elif command -v shasum >/dev/null 2>&1; then
      shasum -a 256 -c "$(basename "$sha_path")"
    else
      die "sha256sum or shasum is required to verify release artifacts"
    fi
  )
fi

validate_archive() {
  members_path="${tmp_dir}/archive.members"
  tar -tzf "$archive_path" > "$members_path"
  [ -s "$members_path" ] || die "archive was empty"
  while IFS= read -r member; do
    case "$member" in
      ""|/*|../*|*/../*|*/..)
        die "unsafe archive member path: $member"
        ;;
    esac
    # Accept both license layouts: pre-relicense tarballs (<=v0.35.0) carry
    # LICENSE-MIT + LICENSE-APACHE, later ones carry LICENSE + NOTICE +
    # THIRD-PARTY-NOTICES.md. Refusing either strands one side of the cutover.
    case "$member" in
      "${stage_name}/" \
        | "${stage_name}/phux" \
        | "${stage_name}/phux-mcp" \
        | "${stage_name}/README.md" \
        | "${stage_name}/LICENSE-MIT" \
        | "${stage_name}/LICENSE-APACHE" \
        | "${stage_name}/LICENSE" \
        | "${stage_name}/NOTICE" \
        | "${stage_name}/THIRD-PARTY-NOTICES.md")
        ;;
      *)
        die "unexpected archive member: $member"
        ;;
    esac
  done < "$members_path"
}

link_count() {
  if stat -f '%l' "$1" >/dev/null 2>&1; then
    stat -f '%l' "$1"
  else
    stat -c '%h' "$1"
  fi
}

validate_extracted_tree() {
  [ -d "$stage_dir" ] && [ ! -L "$stage_dir" ] \
    || die "archive did not contain expected stage directory"

  while IFS= read -r path; do
    rel="${path#"$extract_dir"/}"
    # Same dual-layout acceptance as validate_archive above.
    case "$rel" in
      "${stage_name}" \
        | "${stage_name}/phux" \
        | "${stage_name}/phux-mcp" \
        | "${stage_name}/README.md" \
        | "${stage_name}/LICENSE-MIT" \
        | "${stage_name}/LICENSE-APACHE" \
        | "${stage_name}/LICENSE" \
        | "${stage_name}/NOTICE" \
        | "${stage_name}/THIRD-PARTY-NOTICES.md")
        ;;
      *)
        die "unexpected extracted member: $rel"
        ;;
    esac

    if [ -L "$path" ]; then
      die "archive member must not be a symlink: $rel"
    fi
    if [ -d "$path" ]; then
      continue
    fi
    if [ ! -f "$path" ]; then
      die "archive member must be a regular file or directory: $rel"
    fi
    if [ "$(link_count "$path")" != "1" ]; then
      die "archive member must not be a hard link: $rel"
    fi
  done <<EOF
$(find "$extract_dir" -mindepth 1 -print)
EOF
}

mkdir -p "$extract_dir"
validate_archive
tar -xzf "$archive_path" -C "$extract_dir"
validate_extracted_tree

[ -f "${stage_dir}/phux" ] && [ ! -L "${stage_dir}/phux" ] || die "archive did not contain a regular phux binary"
[ -f "${stage_dir}/phux-mcp" ] && [ ! -L "${stage_dir}/phux-mcp" ] || die "archive did not contain a regular phux-mcp binary"

mkdir -p "$install_dir"
lock_dir="${install_dir}/.phux-install.lock"
# Exclusive mkdir still claims the canonical lock. Ownership is recorded in a
# pid-tagged pending directory first so interruption before lock_acquired=1
# cannot strand our lock or delete another installer's.
lock_pending="${lock_dir}.$$"
rm -rf "$lock_pending" 2>/dev/null || true
mkdir "$lock_pending" || die "could not create install lock"
printf '%s\n' "$$" > "${lock_pending}/pid"
# Hold HUP/INT/TERM across the exclusive mkdir so a signal cannot land after
# the lock exists and before lock_acquired=1. The pending directory still
# covers interruption before this hold.
trap '' HUP INT TERM
if ! mkdir "$lock_dir" 2>/dev/null; then
  trap_install_signals
  rm -rf "$lock_pending"
  lock_pending=""
  die "another phux install is already publishing to ${install_dir}"
fi
lock_acquired=1
mv "${lock_pending}/pid" "${lock_dir}/pid"
rm -rf "$lock_pending"
lock_pending=""
trap_install_signals

# Stage and back up on the destination filesystem so every publish/restore is
# a rename. The trap restores the complete previous pair after any partial
# publication, including interruption between the two renames.
publish_dir="$(mktemp -d "${install_dir}/.phux-install.XXXXXX")"
mkdir "${publish_dir}/backup"
cp "${stage_dir}/phux" "${publish_dir}/phux"
cp "${stage_dir}/phux-mcp" "${publish_dir}/phux-mcp"
chmod 755 "${publish_dir}/phux" "${publish_dir}/phux-mcp"

for installed_name in phux phux-mcp; do
  installed_path="${install_dir}/${installed_name}"
  if [ -e "$installed_path" ] && [ ! -f "$installed_path" ] && [ ! -L "$installed_path" ]; then
    die "refusing to replace non-file install destination: ${installed_path}"
  fi
done

publish_started=1
if [ -e "${install_dir}/phux" ] || [ -L "${install_dir}/phux" ]; then
  mv "${install_dir}/phux" "${publish_dir}/backup/phux"
fi
if [ -e "${install_dir}/phux-mcp" ] || [ -L "${install_dir}/phux-mcp" ]; then
  mv "${install_dir}/phux-mcp" "${publish_dir}/backup/phux-mcp"
fi
# Mark each destination before its rename so an INT/TERM/HUP delivered between
# the rename and the next shell statement still rolls the partial pair back.
published_phux=1
mv "${publish_dir}/phux" "${install_dir}/phux"
published_phux_mcp=1
mv "${publish_dir}/phux-mcp" "${install_dir}/phux-mcp"
publish_complete=1
printf '%s\n' "$channel" > "${install_dir}/.phux-channel"

phux_ok "installed phux ${version} for ${target} to ${install_dir}"
installed_dir="$(cd "$install_dir" && pwd -P)"
installed_command="${installed_dir}/phux"
found_command="$(command -v phux 2>/dev/null || true)"
found_canonical=""
if [ -n "$found_command" ] && [ "${found_command#*/}" != "$found_command" ]; then
  found_dir="$(dirname "$found_command")"
  if found_dir="$(cd "$found_dir" 2>/dev/null && pwd -P)"; then
    found_canonical="${found_dir}/$(basename "$found_command")"
  fi
fi

if [ "$found_canonical" = "$installed_command" ]; then
  echo "next: phux"
  exit 0
fi

# Is the install directory on PATH at all (as opposed to shadowed by an
# earlier phux)? Only a missing entry is fixed in the shell profile.
dir_on_path=0
old_ifs="$IFS"
IFS=:
set -f
for entry in $PATH; do
  [ -n "$entry" ] || continue
  if [ "$(cd "$entry" 2>/dev/null && pwd -P)" = "$installed_dir" ]; then
    dir_on_path=1
    break
  fi
done
set +f
IFS="$old_ifs"

# Append one idempotent PATH line to the profile the user's shell reads. Never
# fatal: an unwritable profile falls back to the printed remedy.
add_to_profile() {
  [ -n "${HOME:-}" ] || return 1
  quoted_dir="$(shell_quote "$installed_dir")"
  line="export PATH=${quoted_dir}:\"\$PATH\""
  case "$(basename "${SHELL:-sh}")" in
    fish)
      profile="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/phux.fish"
      line="fish_add_path -g ${quoted_dir}"
      ;;
    zsh) profile="${ZDOTDIR:-$HOME}/.zshrc" ;;
    bash)
      # macOS terminals start login shells, which read .bash_profile.
      profile="$HOME/.bashrc"
      [ "$(uname -s)" != Darwin ] || profile="$HOME/.bash_profile"
      ;;
    *) profile="$HOME/.profile" ;;
  esac
  if [ -f "$profile" ] && grep -Fqx "$line" "$profile" 2>/dev/null; then
    printf '%s\n' "$profile"
    return 0
  fi
  mkdir -p "$(dirname "$profile")" 2>/dev/null || return 1
  printf '\n# Added by the phux installer\n%s\n' "$line" >> "$profile" 2>/dev/null || return 1
  printf '%s\n' "$profile"
}

printf 'next: %s\n' "$(shell_quote "$installed_command")"
if [ "$dir_on_path" -eq 0 ] && [ "$modify_path" -eq 1 ] && profile="$(add_to_profile)"; then
  printf 'added %s to PATH in %s; new terminals find phux\n' \
    "$(shell_quote "$installed_dir")" "$(shell_quote "$profile")"
fi
# The printed remedy must defer expansion until the user runs it.
# shellcheck disable=SC2016
printf 'PATH remedy: export PATH=%s:"$PATH"\n' "$(shell_quote "$installed_dir")"
