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

usage() {
  cat <<'EOF'
Usage: scripts/install.sh [--version <vX.Y.Z>] [--channel stable|next] [options]

Options:
  --version <vX.Y.Z>       Release tag to install (default: latest GitHub release).
  --channel <stable|next>  Release channel (default: stable, or \$PHUX_CHANNEL).
                           next tracks green main; omit --version with it.
  --install-dir <dir>      Directory for phux and phux-mcp (default: $HOME/.local/bin).
  --os <darwin|linux>      Override OS detection.
  --arch <arm64|aarch64|x86_64|amd64>
                           Override architecture detection.
  --dry-run                Print resolved target, URLs, and install dir only.
  --help                   Show this help.
EOF
}

die() {
  echo "error: $*" >&2
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
# Embedded verbatim in both standalone installers by sync-install-resolver.sh.
# Runtime dependencies are POSIX sh/awk/utilities and curl or wget, never jq or
# Python. Parse JSON structurally: release bodies and nested assets are not tags.

valid_release_tag() {
  case "$1" in *[!A-Za-z0-9.-]*) return 1 ;; esac
  printf '%s\n' "$1" | LC_ALL=C grep -Eq "^${2}(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$"
}

release_page() {
  LC_ALL=C awk -v prefix="$1" '
    function fail() { invalid = 1; exit 1 }
    # Keep unread input in 1 KiB chunks. Matching/removing a small token must
    # not copy the entire page; only a token spanning chunks grows the buffer.
    function append_record(line,    i, size) {
      line = tail line "\n"
      size = length(line)
      for (i = 1; i + 1023 <= size; i += 1024) chunks[++chunk_count] = substr(line, i, 1024)
      tail = substr(line, i)
    }
    function more_input() {
      if (chunk_read == chunk_count) return 0
      buffer = buffer chunks[++chunk_read]
      delete chunks[chunk_read]
      return 1
    }
    function advance(    c) {
      while (1) {
        if (buffer == "" && !more_input()) { kind = ""; text = ""; return }
        match(buffer, /^[ \t\r\n]*/)
        if (RLENGTH == 0) break
        buffer = substr(buffer, RLENGTH + 1)
      }
      c = substr(buffer, 1, 1)
      kind = c; text = c
      if (c == "\"") { string_token(); return }
      if (index("{}[],:", c)) { buffer = substr(buffer, 2); return }
      literal_token()
    }
    function literal_token() {
      # Do not accept a partial number/keyword at a chunk boundary. A complete
      # release array always supplies a delimiter after every scalar value.
      while (!match(buffer, /[ \t\r\n{}\[\],:"]/)) {
        if (!more_input()) fail()
      }
      text = substr(buffer, 1, RSTART - 1)
      buffer = substr(buffer, RSTART)
      if (text !~ /^(-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?|true|false|null)$/) fail()
      kind = "literal"
    }
    function string_token(    c) {
      # Most strings fit in the current chunk. One complete-token match keeps
      # dense arrays fast; failure switches to the incremental path below.
      if (match(buffer, /^"([^"\\[:cntrl:]]|\\(["\\\/bfnrt]|u[0-9a-fA-F]{4}))*"/)) {
        text = substr(buffer, 2, RLENGTH - 2)
        buffer = substr(buffer, RLENGTH + 1)
        kind = "string"; return
      }
      buffer = substr(buffer, 2)
      text = ""; kind = "string"
      while (1) {
        if (buffer == "" && !more_input()) fail()
        # Match only the unread fragment, never the accumulated string. The
        # escape helpers consume exact bytes even across a chunk boundary.
        match(buffer, /^[^"\\[:cntrl:]]+/)
        if (RLENGTH > 0) {
          text = text substr(buffer, 1, RLENGTH)
          buffer = substr(buffer, RLENGTH + 1)
          continue
        }
        c = string_byte()
        if (c == "\"") return
        if (c != "\\") fail()
        text = text "\\" string_escape()
      }
    }
    function string_byte(    c) {
      if (buffer == "" && !more_input()) fail()
      c = substr(buffer, 1, 1)
      buffer = substr(buffer, 2)
      return c
    }
    function string_escape(    c) {
      c = string_byte()
      if (c == "u") return c unicode_escape()
      if (!index("\"\\/bfnrt", c)) fail()
      return c
    }
    function unicode_escape(    i, c, hex) {
      for (i = 0; i < 4; i++) {
        c = string_byte()
        if (c !~ /^[0-9a-fA-F]$/) fail()
        hex = hex c
      }
      return hex
    }
    function consume(expected) {
      if (kind != expected) fail()
      advance()
    }
    # Only keys and tag names need decoding. Their vocabulary is ASCII. Other
    # Unicode remains lexically validated but cannot turn into an ASCII key/tag.
    function ascii_string(raw,    result, i, c) {
      if (!index(raw, "\\")) return raw
      result = ""
      for (i = 1; i <= length(raw); i++) {
        c = substr(raw, i, 1)
        if (c == "\\") {
          i++; c = substr(raw, i, 1)
          if (c == "u") { c = unicode_ascii(substr(raw, i + 1, 4)); i += 4 }
          else if (index("bfnrt", c)) c = "?"
        }
        result = result c
      }
      return result
    }
    function unicode_ascii(hex,    n, i) {
      n = 0
      for (i = 1; i <= 4; i++) n = n * 16 + index("0123456789abcdef", tolower(substr(hex, i, 1))) - 1
      if (n < 32 || n > 126) return "?"
      return sprintf("%c", n)
    }
    function value(depth) {
      if (depth > 128) fail()
      if (kind == "{") { object(depth, 0); return }
      if (kind == "[") { array(depth); return }
      if (kind != "string" && kind != "literal") fail()
      advance()
    }
    function array(depth) {
      consume("[")
      if (kind == "]") { advance(); return }
      while (1) {
        value(depth + 1)
        if (kind == "]") { advance(); return }
        consume(",")
      }
    }
    function object(depth, capture,    key) {
      consume("{")
      if (kind == "}") { advance(); return }
      while (1) {
        if (kind != "string") fail()
        key = ascii_string(text)
        advance(); consume(":")
        if (capture) field(key)
        value(depth + 1)
        if (kind == "}") { advance(); return }
        consume(",")
      }
    }
    function field(key) {
      if (key != "tag_name" && key != "draft" && key != "prerelease") return
      if (key in types) fail()
      types[key] = kind
      fields[key] = text
    }
    function boolean_field(key) {
      return types[key] == "literal" && (fields[key] == "true" || fields[key] == "false")
    }
    function stable_metadata() {
      if (types["tag_name"] != "string") fail()
      if (!boolean_field("draft") || !boolean_field("prerelease")) fail()
      return fields["draft"] == "false" && fields["prerelease"] == "false"
    }
    function release(    key, tag, version_pattern) {
      for (key in fields) delete fields[key]
      for (key in types) delete types[key]
      object(2, 1)
      if (!stable_metadata()) return
      tag = ascii_string(fields["tag_name"])
      version_pattern = "(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)"
      if (selected == "" && tag ~ ("^" prefix version_pattern "$")) selected = tag
    }
    function page() {
      advance(); consume("[")
      if (kind != "]") {
        while (1) {
          release(); count++
          if (kind == "]") break
          consume(",")
        }
      }
      consume("]")
      if (kind != "") fail()
      if (selected != "") print selected
      else if (count == 0) print "empty"
      else print "more"
    }
    { append_record($0) }
    END {
      if (tail != "") chunks[++chunk_count] = tail
      if (!invalid) page()
    }
  ' "$2"
}

fetch_release_page() (
  # A file-size limit also bounds wget and curl versions that only enforce
  # --max-filesize when Content-Length is present. sh implementations use 512
  # or 1024 byte blocks; this ceiling is at most 2 MiB. Check the exact 1 MiB
  # limit after download, before awk reads a potentially huge single line.
  ulimit -f 2048
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --connect-timeout 10 --max-time 30 "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -q --timeout=30 --tries=1 -O "$2" "$1"
  else
    die "curl or wget is required to resolve a release; install one or pass --version"
  fi
)

resolve_latest_version() (
  # GitHub returns release streams interleaved, newest first. Keep all network
  # and temporary state inside this subshell, including for --dry-run.
  prefix="$1"
  index_dir="$(mktemp -d)"
  trap 'rm -rf "$index_dir"' 0
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  page=1
  while [ "$page" -le 10 ]; do
    url="https://api.github.com/repos/no-phux/phux/releases?per_page=30&page=${page}"
    fetch_release_page "$url" "$index_dir/page.json" \
      || die "could not fetch release page $page (limit 1048576 bytes); check GitHub access/rate limits or pass --version"
    [ "$(wc -c < "$index_dir/page.json")" -le 1048576 ] \
      || die "release page exceeds 1048576 bytes; pass --version to select a known release"
    result="$(release_page "$prefix" "$index_dir/page.json")" \
      || die "invalid release list on page $page; retry or pass --version to select a known release"
    case "$result" in
      empty) die "no stable ${prefix}X.Y.Z release found; pass --version to select a known release" ;;
      more) page=$((page + 1)) ;;
      *) printf '%s\n' "$result"; exit 0 ;;
    esac
  done
  die "no stable ${prefix}X.Y.Z release found within 10 pages; pass --version to select a known release"
)
# END shared release resolver
resolve_next_sha() {
  channel_url="https://github.com/no-phux/phux/releases/download/next/channel.json"
  if command -v curl >/dev/null 2>&1; then
    body="$(curl -fsSL "$channel_url")" \
      || die "could not download the next channel pointer"
  elif command -v wget >/dev/null 2>&1; then
    body="$(wget -qO- "$channel_url")" \
      || die "could not download the next channel pointer"
  else
    die "curl or wget is required to resolve the next channel"
  fi
  next_sha="$(printf '%s\n' "$body" \
    | sed -n 's/.*"sha"[[:space:]]*:[[:space:]]*"\([0-9a-f]\{40\}\)".*/\1/p' \
    | head -n 1)"
  [ -n "$next_sha" ] || die "the next channel pointer named no SHA"
  printf '%s\n' "$next_sha"
}

if [ -z "$channel" ]; then
  channel="${PHUX_CHANNEL:-stable}"
fi
case "$channel" in
  stable|next) ;;
  *) die "--channel must be stable or next" ;;
esac

if [ "$channel" = "next" ] && [ -n "$version" ]; then
  die "--version pins a stable tag; omit it when using --channel next"
fi

release_tag=""
artifact_id=""
if [ "$channel" = "next" ]; then
  next_sha="$(resolve_next_sha)"
  release_tag="next"
  artifact_id="next.${next_sha}"
  version="next.${next_sha}"
else
  if [ -z "$version" ]; then
    version="$(resolve_latest_version v)"
  fi
  valid_release_tag "$version" v \
    || die "--version must be a release like vX.Y.Z (got ${version})"
  release_tag="$version"
  artifact_id="$version"
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

base_url="https://github.com/no-phux/phux/releases/download/${release_tag}"
artifact="phux-${artifact_id}-${target}.tar.gz"
archive_url="${base_url}/${artifact}"
sha_url="${archive_url}.sha256"

if [ "$dry_run" -eq 1 ]; then
  echo "channel: ${channel}"
  echo "target: ${target}"
  echo "archive_url: ${archive_url}"
  echo "sha256_url: ${sha_url}"
  echo "install_dir: ${install_dir}"
  exit 0
fi

if command -v curl >/dev/null 2>&1; then
  download() {
    curl -fsSL "$1" -o "$2"
  }
elif command -v wget >/dev/null 2>&1; then
  download() {
    wget -q -O "$2" "$1"
  }
else
  die "curl or wget is required to download release artifacts"
fi

tmp_dir="$(mktemp -d)"
publish_dir=""
lock_dir=""
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

cleanup() {
  rollback_publish
  if [ -n "$publish_dir" ]; then
    rm -rf "$publish_dir"
  fi
  if [ "$lock_acquired" -eq 1 ]; then
    rmdir "$lock_dir" 2>/dev/null || true
  fi
  rm -rf "$tmp_dir"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

archive_path="${tmp_dir}/${artifact}"
sha_path="${archive_path}.sha256"
extract_dir="${tmp_dir}/extract"
stage_dir="${extract_dir}/phux-${artifact_id}-${target}"
stage_name="phux-${artifact_id}-${target}"

download "$archive_url" "$archive_path"
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
    case "$member" in
      "${stage_name}/" \
        | "${stage_name}/phux" \
        | "${stage_name}/phux-mcp" \
        | "${stage_name}/README.md" \
        | "${stage_name}/LICENSE-MIT" \
        | "${stage_name}/LICENSE-APACHE" \
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
    case "$rel" in
      "${stage_name}" \
        | "${stage_name}/phux" \
        | "${stage_name}/phux-mcp" \
        | "${stage_name}/README.md" \
        | "${stage_name}/LICENSE-MIT" \
        | "${stage_name}/LICENSE-APACHE" \
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
if ! mkdir "$lock_dir" 2>/dev/null; then
  die "another phux install is already publishing to ${install_dir}"
fi
lock_acquired=1

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
# Mark each destination before its rename so an INT/TERM delivered between
# the rename and the next shell statement still rolls the partial pair back.
published_phux=1
mv "${publish_dir}/phux" "${install_dir}/phux"
published_phux_mcp=1
mv "${publish_dir}/phux-mcp" "${install_dir}/phux-mcp"
publish_complete=1
printf '%s\n' "$channel" > "${install_dir}/.phux-channel"

echo "installed phux ${version} for ${target} to ${install_dir}"
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
else
  printf 'next: %s\n' "$(shell_quote "$installed_command")"
  # The printed remedy must defer expansion until the user runs it.
  # shellcheck disable=SC2016
  printf 'PATH remedy: export PATH=%s:"$PATH"\n' "$(shell_quote "$installed_dir")"
fi
