#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/install.sh [--version <vX.Y.Z>] [options]

Options:
  --version <vX.Y.Z>       Release tag to install (default: latest GitHub release).
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

version=""
install_dir="${PHUX_INSTALL_DIR:-${HOME:-}/.local/bin}"
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

resolve_latest_version() {
  latest=""
  if command -v curl >/dev/null 2>&1; then
    latest_url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
      https://github.com/phall1/phux/releases/latest)" \
      || die "could not resolve latest GitHub release"
    latest="${latest_url##*/}"
  elif command -v wget >/dev/null 2>&1; then
    latest_json="$(wget -qO- https://api.github.com/repos/phall1/phux/releases/latest)" \
      || die "could not resolve latest GitHub release"
    latest="$(printf '%s\n' "$latest_json" \
      | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
      | head -n 1)"
  else
    die "curl or wget is required to resolve the latest release"
  fi
  [ -n "$latest" ] || die "latest GitHub release did not include a tag"
  printf '%s\n' "$latest"
}

if [ -z "$version" ]; then
  version="$(resolve_latest_version)"
fi

case "$version" in
  v*) ;;
  *) die "--version must be a release tag like vX.Y.Z" ;;
esac

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

base_url="https://github.com/phall1/phux/releases/download/${version}"
artifact="phux-${version}-${target}.tar.gz"
archive_url="${base_url}/${artifact}"
sha_url="${archive_url}.sha256"

if [ "$dry_run" -eq 1 ]; then
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
stage_dir="${extract_dir}/phux-${version}-${target}"
stage_name="phux-${version}-${target}"

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
        | "${stage_name}/LICENSE-APACHE")
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
        | "${stage_name}/LICENSE-APACHE")
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
  printf 'next: %q\n' "$installed_command"
  # The printed remedy must defer expansion until the user runs it.
  # shellcheck disable=SC2016
  printf 'PATH remedy: export PATH=%q:"$PATH"\n' "$installed_dir"
fi
