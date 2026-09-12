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
    function string_token() {
      while (!match(buffer, /^"([^"\\[:cntrl:]]|\\(["\\\/bfnrt]|u[0-9a-fA-F]{4}))*"/)) {
        if (!more_input()) fail()
      }
      text = substr(buffer, 2, RLENGTH - 2)
      buffer = substr(buffer, RLENGTH + 1)
      kind = "string"
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
