#!/bin/sh
#
# POSIX sh on purpose. This script is served verbatim at
# https://phux.sh/install-cockpit and has to survive being piped to `sh`.
# Two bashisms in particular are fatal there and must not come back:
# `set -o pipefail` (rejected by dash before 0.5.12) and `printf %q` (an
# invalid directive in every dash). `just shellcheck` lints this file as sh
# because of the shebang above, so a new bashism fails the gate rather than a
# stranger's install.
#
# What it does: downloads the `phux-cockpit-<semver>-macos-arm64.zip` asset
# from the latest `cockpit-vX.Y.Z` GitHub release, verifies it against the
# release SHA256SUMS, and places `Phux Cockpit.app` in /Applications (or
# ~/Applications when /Applications is not writable). The quarantine attribute
# is cleared, the same step the Homebrew cask performs for ad-hoc-signed
# builds. An existing install is backed up and restored if placement fails.
set -eu

usage() {
  cat <<'EOF'
Usage: scripts/install-cockpit.sh [--version <cockpit-vX.Y.Z>] [options]

Options:
  --version <cockpit-vX.Y.Z|X.Y.Z>
                         Cockpit release to install (default: latest GitHub release).
  --applications-dir <dir>
                         Directory for Phux Cockpit.app (default: /Applications
                         when writable, else $HOME/Applications).
  --os <darwin>          Override OS detection (Cockpit is macOS-only).
  --arch <arm64|aarch64> Override architecture detection.
  --dry-run              Print resolved tag, URLs, and destination only.
  --help                 Show this help.
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
applications_dir="${PHUX_COCKPIT_APPLICATIONS_DIR:-}"
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
    --applications-dir)
      [ "$#" -ge 2 ] || die "--applications-dir requires a value"
      applications_dir="$2"
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

if [ -z "$version" ]; then
  version="$(resolve_latest_version cockpit-v)"
fi

# Accept a bare semver as shorthand; the release tag carries the prefix.
case "$version" in
  cockpit-v*) semver="${version#cockpit-v}" ;;
  *) semver="$version"; version="cockpit-v$semver" ;;
esac
valid_release_tag "$version" cockpit-v \
  || die "--version must be a release like cockpit-vX.Y.Z (got ${version})"

if [ -z "$os" ]; then
  case "$(uname -s)" in
    Darwin) os="darwin" ;;
    *) die "Phux Cockpit is macOS-only; this machine reports $(uname -s)" ;;
  esac
fi
[ "$os" = "darwin" ] || die "Phux Cockpit is macOS-only (got --os $os)"

if [ -z "$arch" ]; then
  arch="$(uname -m)"
fi
case "$arch" in
  arm64|aarch64) ;;
  *) die "Phux Cockpit ships arm64 only; this machine reports $arch" ;;
esac

if [ -z "$applications_dir" ]; then
  if [ -d "/Applications" ] && [ -w "/Applications" ]; then
    applications_dir="/Applications"
  else
    applications_dir="${HOME:-}/Applications"
  fi
fi
[ -n "$applications_dir" ] || die "--applications-dir resolved to an empty path"

base_url="https://github.com/no-phux/phux/releases/download/${version}"
zip_name="phux-cockpit-${semver}-macos-arm64.zip"
zip_url="${base_url}/${zip_name}"
sums_url="${base_url}/SHA256SUMS"

if [ "$dry_run" -eq 1 ]; then
  echo "tag: ${version}"
  echo "zip_url: ${zip_url}"
  echo "sha256_url: ${sums_url}"
  echo "applications_dir: ${applications_dir}"
  exit 0
fi

for tool in unzip ditto xattr; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is required to install Phux Cockpit"
done

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
published_app=0

rollback_publish() {
  [ "$publish_started" -eq 1 ] || return 0
  [ "$publish_complete" -eq 0 ] || return 0
  if [ "$published_app" -eq 1 ]; then
    rm -rf "${applications_dir}/Phux Cockpit.app"
  fi
  if [ -e "${publish_dir}/backup/Phux Cockpit.app" ] || [ -L "${publish_dir}/backup/Phux Cockpit.app" ]; then
    mv -f "${publish_dir}/backup/Phux Cockpit.app" "${applications_dir}/Phux Cockpit.app"
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

zip_path="${tmp_dir}/${zip_name}"
sums_path="${tmp_dir}/SHA256SUMS"
extract_dir="${tmp_dir}/extract"

download "$zip_url" "$zip_path"
download "$sums_url" "$sums_path"

# The release checksums cover the zip and the dmg; only the zip line matters
# here, and requiring the whole file to verify would demand the dmg too.
awk -v name="$zip_name" '$2 == name' "$sums_path" > "${tmp_dir}/cockpit.sha256" \
  || die "SHA256SUMS did not cover ${zip_name}"
[ -s "${tmp_dir}/cockpit.sha256" ] || die "SHA256SUMS did not cover ${zip_name}"
(
  cd "$tmp_dir"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "cockpit.sha256"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c "cockpit.sha256"
  else
    die "sha256sum or shasum is required to verify release artifacts"
  fi
)

validate_zip() {
  members_path="${tmp_dir}/zip.members"
  # -Z1 lists one filename per line; `unzip -l` column parsing would split
  # the space in "Phux Cockpit.app".
  unzip -Z1 "$zip_path" | sed -n '/./p' > "$members_path"
  [ -s "$members_path" ] || die "archive was empty"
  while IFS= read -r member; do
    case "$member" in
      ""|/*|../*|*/../*) die "unsafe archive member path: $member" ;;
    esac
    case "$member" in
      "Phux Cockpit.app/"*) ;;
      *) die "unexpected archive member: $member" ;;
    esac
  done < "$members_path"
  grep -Fxq "Phux Cockpit.app/Contents/Info.plist" "$members_path" \
    || die "archive did not contain a Phux Cockpit.app bundle"
}

mkdir -p "$extract_dir"
validate_zip
unzip -q -o "$zip_path" -d "$extract_dir"
[ -d "${extract_dir}/Phux Cockpit.app" ] && [ ! -L "${extract_dir}/Phux Cockpit.app" ] \
  || die "archive did not contain a Phux Cockpit.app directory"
[ -f "${extract_dir}/Phux Cockpit.app/Contents/Info.plist" ] \
  || die "archive did not contain a Phux Cockpit.app bundle"

mkdir -p "$applications_dir"
lock_dir="${applications_dir}/.phux-cockpit-install.lock"
if ! mkdir "$lock_dir" 2>/dev/null; then
  die "another Cockpit install is already publishing to ${applications_dir}"
fi
lock_acquired=1

publish_dir="$(mktemp -d "${applications_dir}/.phux-cockpit-install.XXXXXX")"
mkdir "${publish_dir}/backup"
# Finish copying on the destination filesystem before touching an existing app.
# Only renames happen during publication, so a failed copy leaves it available.
ditto "${extract_dir}/Phux Cockpit.app" "${publish_dir}/Phux Cockpit.app"

installed_path="${applications_dir}/Phux Cockpit.app"
if [ -e "$installed_path" ] && [ ! -d "$installed_path" ]; then
  die "refusing to replace non-directory install destination: ${installed_path}"
fi

publish_started=1
if [ -e "$installed_path" ] || [ -L "$installed_path" ]; then
  mv "$installed_path" "${publish_dir}/backup/Phux Cockpit.app"
fi
# Mark before mv so interruption immediately after the rename still rolls back.
# A failed backup rename never sets this flag: the original remains untouched.
published_app=1
mv "${publish_dir}/Phux Cockpit.app" "$installed_path"
publish_complete=1

# Releases without Developer ID credentials are ad-hoc signed; clearing the
# quarantine attribute is what lets such a build launch, and is the same step
# the Homebrew cask performs.
xattr -d com.apple.quarantine "$installed_path" 2>/dev/null || true

echo "installed Phux Cockpit ${version} to ${applications_dir}"
installed_app="$(CDPATH='' cd "$applications_dir" && pwd -P)/Phux Cockpit.app"
printf 'next: open %s\n' "$(shell_quote "$installed_app")"
