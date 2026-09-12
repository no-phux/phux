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

resolve_latest_version() {
  list_url="https://api.github.com/repos/no-phux/phux/releases?per_page=30"
  if command -v curl >/dev/null 2>&1; then
    list="$(curl -fsSL "$list_url")" \
      || die "could not list GitHub releases"
  elif command -v wget >/dev/null 2>&1; then
    list="$(wget -qO- "$list_url")" \
      || die "could not list GitHub releases"
  else
    die "curl or wget is required to resolve the latest release"
  fi
  latest="$(printf '%s\n' "$list" \
    | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\(cockpit-v[0-9][^"]*\)".*/\1/p' \
    | head -n 1)"
  [ -n "$latest" ] || die "no cockpit release found in recent GitHub releases"
  printf '%s\n' "$latest"
}

if [ -z "$version" ]; then
  version="$(resolve_latest_version)"
fi

# Accept a bare semver as shorthand; the release tag carries the prefix.
case "$version" in
  cockpit-v*) semver="${version#cockpit-v}" ;;
  *) semver="$version"; version="cockpit-v$semver" ;;
esac
case "$semver" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) die "--version must be a release like cockpit-vX.Y.Z (got ${version})" ;;
esac
case "$semver" in
  *[!0-9.]*) die "--version must be a release like cockpit-vX.Y.Z (got ${version})" ;;
esac

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
echo 'next: open -a "Phux Cockpit"'
