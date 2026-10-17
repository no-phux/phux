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
    curl -fsSL --proto =https --connect-timeout 10 --speed-limit 1024 --speed-time 60 "$1" -o "$2"
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
  download "$asset_url" "${work}/${asset_name}" \
    || die "could not download ${asset_name}; nothing was installed"
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

# --- the CLI -----------------------------------------------------------------

LICENSES='README.md LICENSE NOTICE THIRD-PARTY-NOTICES.md LICENSE-MIT LICENSE-APACHE'

# A tarball member under STEM: the directory, a binary, or a license file
# (both the current and the pre-0.36 layout).
cli_member() {
  [ "$2" = "$1" ] || [ "$2" = "$1/" ] && return 0
  for allowed in phux phux-mcp $LICENSES; do
    [ "$2" != "$1/$allowed" ] || return 0
  done
  return 1
}

# GNU stat reads -f as "file system", so try its -c first.
link_count() {
  if stat -c '%h' "$1" >/dev/null 2>&1; then stat -c '%h' "$1"; else stat -f '%l' "$1"; fi
}

# Unpack the verified tarball into the work directory; refuse any member a
# release does not contain, and any link.
unpack_cli() {
  tar -tzf "${work}/${asset_name}" > "${work}/members"
  [ -s "${work}/members" ] || die "the archive is empty"
  while IFS= read -r member; do
    cli_member "$asset_stem" "$member" || die "unexpected archive member: $member"
  done < "${work}/members"
  mkdir "${work}/cli"
  tar -xzf "${work}/${asset_name}" -C "${work}/cli"
  find "${work}/cli" -mindepth 1 -print | while IFS= read -r path; do
    rel="${path#"${work}/cli"/}"
    cli_member "$asset_stem" "$rel" || die "unexpected extracted member: $rel"
    [ ! -L "$path" ] || die "archive member must not be a symlink: $rel"
    [ -d "$path" ] || { [ -f "$path" ] && [ "$(link_count "$path")" = 1 ]; } \
      || die "archive member must be a regular file: $rel"
  done
  for binary in phux phux-mcp; do
    [ -f "${work}/cli/${asset_stem}/${binary}" ] || die "the archive has no ${binary}"
  done
}

# Publish phux and phux-mcp into install_dir by rename, restoring the
# previous pair if anything is interrupted.
publish_cli() {
  mkdir -p "$install_dir"
  cli_lock="${install_dir}/.phux-install.lock"
  trap '' HUP INT TERM
  mkdir "$cli_lock" 2>/dev/null \
    || die "another phux install is publishing to ${install_dir}; if none is running, remove ${cli_lock}"
  cli_locked=1
  trap_signals
  cli_publish="$(mktemp -d "${install_dir}/.phux-install.XXXXXX")"
  mkdir "${cli_publish}/backup"
  for binary in phux phux-mcp; do
    target_path="${install_dir}/${binary}"
    if [ -e "$target_path" ] && [ ! -f "$target_path" ] && [ ! -L "$target_path" ]; then
      die "refusing to replace non-file install destination: ${target_path}"
    fi
    cp "${work}/cli/${asset_stem}/${binary}" "${cli_publish}/${binary}"
    chmod 755 "${cli_publish}/${binary}"
  done
  cli_publishing=1
  for binary in phux phux-mcp; do
    if [ -e "${install_dir}/${binary}" ] || [ -L "${install_dir}/${binary}" ]; then
      mv "${install_dir}/${binary}" "${cli_publish}/backup/${binary}"
    fi
  done
  for binary in phux phux-mcp; do
    # Recorded before the rename, so a signal right after it still rolls back.
    cli_published="$cli_published $binary"
    mv "${cli_publish}/${binary}" "${install_dir}/${binary}"
  done
  cli_publishing=0
  printf '%s\n' "$channel" > "${install_dir}/.phux-channel"
}

rollback_cli() {
  [ "$cli_publishing" -eq 1 ] || return 0
  for binary in $cli_published; do rm -f "${install_dir}/${binary}"; done
  for binary in phux phux-mcp; do
    if [ -e "${cli_publish}/backup/${binary}" ] || [ -L "${cli_publish}/backup/${binary}" ]; then
      mv -f "${cli_publish}/backup/${binary}" "${install_dir}/${binary}"
    fi
  done
}

# Append one idempotent PATH line to the profile the user's shell reads, and
# print it. Never fatal: the printed remedy covers a profile it cannot write.
add_to_profile() {
  [ -n "${HOME:-}" ] || return 1
  quoted="$(shell_quote "$1")"
  line="export PATH=${quoted}:\"\$PATH\""
  case "$(basename "${SHELL:-sh}")" in
    fish) profile="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/phux.fish" line="fish_add_path -g ${quoted}" ;;
    zsh) profile="${ZDOTDIR:-$HOME}/.zshrc" ;;
    # macOS terminals start login shells, which read .bash_profile.
    bash) if [ "$(uname -s)" = Darwin ]; then profile="$HOME/.bash_profile"; else profile="$HOME/.bashrc"; fi ;;
    *) profile="$HOME/.profile" ;;
  esac
  if ! grep -Fqx "$line" "$profile" 2>/dev/null; then
    mkdir -p "$(dirname "$profile")" 2>/dev/null || return 1
    printf '\n# Added by the phux installer\n%s\n' "$line" >> "$profile" 2>/dev/null || return 1
  fi
  printf '%s\n' "$profile"
}

report_path() {
  dir="$(cd "$install_dir" && pwd -P)"
  found="$(command -v phux 2>/dev/null || true)"
  if [ "${found#/}" != "$found" ] && [ "$(cd "$(dirname "$found")" && pwd -P)/phux" = "${dir}/phux" ]; then
    echo "next: phux"
    return
  fi
  printf 'next: %s\n' "$(shell_quote "${dir}/phux")"
  on_path=0
  old_ifs="$IFS"
  IFS=:
  set -f
  for entry in $PATH; do
    [ "$(cd "$entry" 2>/dev/null && pwd -P)" != "$dir" ] || on_path=1
  done
  set +f
  IFS="$old_ifs"
  if [ "$on_path" -eq 0 ] && [ "$modify_path" -eq 1 ] && profile="$(add_to_profile "$dir")"; then
    printf 'added %s to PATH in %s; new terminals find phux\n' "$(shell_quote "$dir")" "$(shell_quote "$profile")"
  fi
  # shellcheck disable=SC2016 # the remedy expands when the user runs it
  printf 'PATH remedy: export PATH=%s:"$PATH"\n' "$(shell_quote "$dir")"
}

# PIN and PIN_SHA256 pin the CLI; empty follows the channel.
install_cli() {
  resolve phux "$target" "$1" "$2"
  name_asset phux "$target"
  if [ "$dry_run" -eq 1 ]; then
    print_plan phux "$install_dir"
    return
  fi
  fetch_verified
  unpack_cli
  publish_cli
  phux_message ok "installed phux ${build_id} for ${target} to ${install_dir}"
}

# --- Phux Desktop ------------------------------------------------------------

plist() {
  /usr/libexec/PlistBuddy -c "Print :$2" "$1/Contents/Info.plist" 2>/dev/null
}

# Unzip the verified archive and check it is the signed bundle that was asked for.
unpack_desktop() {
  unzip -Z1 "${work}/${asset_name}" > "${work}/members" || die "could not read the archive"
  # The bundle has no symlinks; refuse one before it can redirect extraction.
  unzip -Z -l "${work}/${asset_name}" | awk '$1 ~ /^l/ { found = 1 } END { exit found }' \
    || die "the archive contains symlinks"
  while IFS= read -r member; do
    case "$member" in
      */../* | */..) die "unsafe archive member path: $member" ;;
      Phux.app/*) ;;
      *) die "unexpected archive member: $member" ;;
    esac
  done < "${work}/members"
  grep -Fxq Phux.app/Contents/Info.plist "${work}/members" || die "the archive has no Phux.app"
  mkdir "${work}/desktop"
  unzip -q "${work}/${asset_name}" -d "${work}/desktop"
  app="${work}/desktop/Phux.app"
  [ -x "${app}/Contents/MacOS/phux-desktop" ] || die "the archive has no desktop executable"
  codesign --verify --deep --strict "$app" || die "the desktop app's signature does not verify"
  [ "$(plist "$app" CFBundleIdentifier)" = dev.phux.desktop ] || die "the archive is not Phux Desktop"
  if [ "$channel" = next ]; then
    [ "$(plist "$app" PhuxBuildSHA)" = "$build_id" ] || die "the bundle is not commit ${build_id}"
    [ "$(plist "$app" PhuxDesktopChannel)" = next ] || die "the bundle is not a next build"
  else
    [ "$(plist "$app" PhuxDesktopVersion)" = "$build_id" ] || die "the bundle is not version ${build_id}"
    bundle_channel="$(plist "$app" PhuxDesktopChannel || true)"
    [ -z "$bundle_channel" ] || [ "$bundle_channel" = stable ] || die "the bundle is not a stable build"
  fi
}

# Every publisher of Phux.app, the running app included, locks this inode.
# The descriptor holds the lock until the shell exits; a killed process
# releases it, so there is no stale lock to recover.
lock_applications() {
  mkdir -p "$applications_dir"
  exec 9>"${applications_dir}/.phux-desktop-install.lock"
  lockf -s -t 0 9 || die "another Desktop install is publishing to ${applications_dir}"
}

# Stage beside the installed app and touch nothing else; the running app swaps
# the staged bundle in after it exits (ADR-0175 decision 7).
stage_desktop() {
  mkdir -p "$stage_dir"
  stage_tmp="$(mktemp -d "${stage_dir}/.staging.XXXXXX")"
  ditto "$app" "${stage_tmp}/Phux.app"
  if [ -e "${stage_dir}/Phux.app" ] || [ -L "${stage_dir}/Phux.app" ]; then
    mv "${stage_dir}/Phux.app" "${stage_tmp}/previous.app"
  fi
  mv "${stage_tmp}/Phux.app" "${stage_dir}/Phux.app"
  echo "staged: ${stage_dir}/Phux.app"
}

publish_desktop() {
  installed="${applications_dir}/Phux.app"
  if [ -e "$installed" ] && [ ! -d "$installed" ]; then
    die "refusing to replace non-directory install destination: ${installed}"
  fi
  if [ -d "$installed" ] && [ "$(plist "$installed" CFBundleIdentifier || true)" != dev.phux.desktop ]; then
    die "refusing to replace an unrelated ${installed}"
  fi
  # Copy onto the destination volume first, so publishing is two renames.
  app_publish="$(mktemp -d "${applications_dir}/.phux-desktop-install.XXXXXX")"
  ditto "$app" "${app_publish}/Phux.app"
  app_publishing=1
  if [ -e "$installed" ]; then mv "$installed" "${app_publish}/previous.app"; fi
  app_published=1
  mv "${app_publish}/Phux.app" "$installed"
  app_publishing=0
  # Ad-hoc signed builds launch only without quarantine; the cask does the same.
  xattr -d com.apple.quarantine "$installed" 2>/dev/null || true
}

rollback_desktop() {
  [ "$app_publishing" -eq 1 ] || return 0
  [ "$app_published" -eq 0 ] || rm -rf "$installed"
  [ ! -d "${app_publish}/previous.app" ] || mv -f "${app_publish}/previous.app" "$installed" || {
    phux_message error "rollback failed; the previous app is at ${app_publish}/previous.app"
    app_publish=""
  }
}

# A PATH launcher that opens the bundle through Launch Services, so the Dock
# identity stays this copy.
write_launcher() {
  mkdir -p "$install_dir" || return 1
  launcher="$(mktemp "${install_dir}/.phux-desktop.XXXXXX")" || return 1
  cat > "$launcher" <<EOF || return 1
#!/bin/sh
# Installed by the Phux installer.
APP=$(shell_quote "$1")
if [ ! -f "\$APP/Contents/Info.plist" ]; then
  echo "phux: error: Phux is not installed at \$APP" >&2
  echo "  hint: curl -fsSL https://phux.sh/install-desktop | sh" >&2
  exit 1
fi
exec open "\$APP"
EOF
  chmod 755 "$launcher" && mv -f "$launcher" "${install_dir}/phux-desktop"
}

# The CLI the app finds from the Dock, where no interactive PATH applies.
has_cli() {
  for candidate in "${install_dir}/phux" "${HOME:-}/.local/bin/phux" "${HOME:-}/.cargo/bin/phux" /opt/homebrew/bin/phux /usr/local/bin/phux /usr/bin/phux; do
    [ ! -x "$candidate" ] || return 0
  done
  command -v phux >/dev/null 2>&1
}

install_desktop() {
  case "$target" in
    *-linux-gnu) install_linux_desktop; return ;;
  esac
  if [ "$dry_run" -eq 0 ]; then
    [ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ] || die "Phux Desktop requires Apple silicon macOS or GNU/Linux"
    [ "$(/usr/bin/sw_vers -productVersion | cut -d. -f1)" -ge 27 ] || die "Phux Desktop requires macOS 27 or later"
  fi
  if [ -z "$stage_dir" ] && ! has_cli; then
    phux_message info "phux CLI not found; installing it first"
    install_cli "" ""
  fi
  resolve desktop aarch64-apple-darwin "$pin" "$pin_sha256"
  name_asset desktop aarch64-apple-darwin
  if [ "$dry_run" -eq 1 ]; then
    print_plan desktop "${stage_dir:-$applications_dir}"
    return
  fi
  for tool in unzip ditto codesign lockf; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is required to install Phux Desktop"
  done
  fetch_verified
  unpack_desktop
  lock_applications
  if [ -n "$stage_dir" ]; then
    stage_desktop
    return
  fi
  publish_desktop
  phux_message ok "installed Phux ${build_id} to ${applications_dir}"
  if write_launcher "$(cd "$applications_dir" && pwd -P)/Phux.app"; then
    echo "launcher: $(cd "$install_dir" && pwd -P)/phux-desktop"
  else
    phux_message warn "could not install the phux-desktop launcher; open the app directly"
  fi
  printf 'next: open %s\n' "$(shell_quote "$(cd "$applications_dir" && pwd -P)/Phux.app")"
}

# Linux archives contain only these regular files/directories. Check types
# before extraction: a symlink must never redirect a later archive member.
linux_data_home() {
  case "${XDG_DATA_HOME:-}" in
    /*) printf '%s\n' "$XDG_DATA_HOME" ;;
    *) printf '%s\n' "${HOME:-}/.local/share" ;;
  esac
}

linux_desktop_member() {
  case "${1%/}" in
    phux-desktop | phux-desktop/bin | phux-desktop/bin/phux-desktop | \
    phux-desktop/share | phux-desktop/share/phux-desktop | \
    phux-desktop/share/phux-desktop/release.json | \
    phux-desktop/share/phux-desktop/LICENSE | phux-desktop/share/phux-desktop/NOTICE | \
    phux-desktop/share/phux-desktop/THIRD-PARTY-NOTICES.txt | \
    phux-desktop/share/phux-desktop/PaperMono-OFL.txt | phux-desktop/share/phux-desktop/JetBrainsMono-OFL.txt | \
    phux-desktop/share/applications | phux-desktop/share/applications/dev.phux.desktop.desktop | \
    phux-desktop/share/icons | phux-desktop/share/icons/hicolor | \
    phux-desktop/share/icons/hicolor/scalable | phux-desktop/share/icons/hicolor/scalable/apps | \
    phux-desktop/share/icons/hicolor/scalable/apps/dev.phux.desktop.svg) return 0 ;;
    *) return 1 ;;
  esac
}

linux_metadata() {
  [ -f "$1/share/phux-desktop/release.json" ] || return 1
  manifest_flat "$1/share/phux-desktop/release.json" > "${work}/desktop-metadata" || return 1
  [ "$(manifest_value "${work}/desktop-metadata" identifier)" = dev.phux.desktop ]
}

check_linux_desktop_archive() {
  tar -tzf "${work}/${asset_name}" > "${work}/members" || die "could not read the archive"
  [ -s "${work}/members" ] || die "the archive is empty"
  tar -tvzf "${work}/${asset_name}" > "${work}/types" || die "could not read archive types"
  LC_ALL=C awk 'substr($0,1,1) !~ /[-d]/ { bad=1 } END { exit bad }' "${work}/types" \
    || die "desktop archive members must be regular files or directories, not links"
  while IFS= read -r member; do
    linux_desktop_member "$member" || die "unexpected archive member: $member"
  done < "${work}/members"
}

check_linux_desktop_release() {
  [ -f "${app}/bin/phux-desktop" ] && [ -x "${app}/bin/phux-desktop" ] || die "the archive has no desktop executable"
  linux_metadata "$app" || die "the archive is not Phux Desktop"
  [ "$(manifest_value "${work}/desktop-metadata" target)" = "$target" ] || die "the bundle is for another target"
  [ "$(manifest_value "${work}/desktop-metadata" channel)" = "$channel" ] || die "the bundle is for another channel"
  metadata_key=version
  [ "$channel" != next ] || metadata_key=sha
  [ "$(manifest_value "${work}/desktop-metadata" "$metadata_key")" = "$build_id" ] || die "the bundle is not build ${build_id}"
}

unpack_linux_desktop() {
  check_linux_desktop_archive
  mkdir "${work}/desktop"
  tar -xzf "${work}/${asset_name}" -C "${work}/desktop" --no-same-owner
  app="${work}/desktop/phux-desktop"
  check_linux_desktop_release
}

publish_linux_desktop() {
  mkdir -p "$applications_dir"
  app_lock="${applications_dir}/.phux-desktop-install.lock"
  trap '' HUP INT TERM
  mkdir "$app_lock" 2>/dev/null || die "another Desktop install is publishing; if none is running, remove ${app_lock}"
  app_locked=1
  trap_signals
  installed="${applications_dir}/phux-desktop"
  [ ! -L "$installed" ] || die "refusing to replace a symlink: ${installed}"
  if [ -e "$installed" ]; then
    linux_metadata "$installed" || die "refusing to replace an unrelated ${installed}"
  fi
  app_publish="$(mktemp -d "${applications_dir}/.phux-desktop-install.XXXXXX")"
  cp -R "$app" "${app_publish}/phux-desktop"
  app_publishing=1
  if [ -e "$installed" ]; then mv "$installed" "${app_publish}/previous.app"; fi
  app_published=1
  mv "${app_publish}/phux-desktop" "$installed"
  app_publishing=0
}

linux_desktop_launcher() {
  mkdir -p "$install_dir" || return 1
  [ ! -d "${install_dir}/phux-desktop" ] || return 1
  launcher="$(mktemp -d "${install_dir}/.phux-desktop.XXXXXX")" || return 1
  if ! ln -s "$(cd "$installed" && pwd -P)/bin/phux-desktop" "${launcher}/phux-desktop"; then
    rmdir "$launcher"
    return 1
  fi
  if mv -f "${launcher}/phux-desktop" "${install_dir}/phux-desktop"; then
    rmdir "$launcher"
  else
    rm -rf "$launcher"
    return 1
  fi
}

# Desktop launchers do not inherit an interactive shell's PATH. Rewrite only
# Exec and Icon to absolute paths, with both desktop-string and Exec quoting.
register_linux_desktop() {
  desktop_source="${installed}/share/applications/dev.phux.desktop.desktop"
  [ -f "$desktop_source" ] || return 1
  bundle_path="$(cd "$installed" && pwd -P)" || return 1
  case "$bundle_path" in *'
'*) return 1 ;; esac
  executable="$(printf '%s' "${bundle_path}/bin/phux-desktop" | sed 's/%/%%/g; s/[\\"`$]/\\&/g; s/\\/\\\\/g')"
  icon="$(printf '%s' "${bundle_path}/share/icons/hicolor/scalable/apps/dev.phux.desktop.svg" | sed 's/\\/\\\\/g')"
  entries="$(linux_data_home)/applications"
  mkdir -p "$entries" || return 1
  [ ! -d "${entries}/dev.phux.desktop.desktop" ] || return 1
  entry="$(mktemp "${entries}/.phux-desktop.XXXXXX")" || return 1
  if PHUX_DESKTOP_EXEC="\"${executable}\"" PHUX_DESKTOP_ICON="$icon" awk '
    /^Exec=/ { print "Exec=" ENVIRON["PHUX_DESKTOP_EXEC"]; next }
    /^Icon=/ { print "Icon=" ENVIRON["PHUX_DESKTOP_ICON"]; next }
    { print }
  ' "$desktop_source" > "$entry" && chmod 644 "$entry" && mv -f "$entry" "${entries}/dev.phux.desktop.desktop"; then
    return 0
  fi
  rm -f "$entry"
  return 1
}

install_linux_desktop() {
  [ -z "$stage_dir" ] || die "automatic Desktop staging is not supported on Linux; install with --desktop instead"
  if ! has_cli; then
    phux_message info "phux CLI not found; installing it first"
    install_cli "" ""
  fi
  resolve desktop "$target" "$pin" "$pin_sha256"
  name_asset desktop "$target"
  if [ "$dry_run" -eq 1 ]; then print_plan desktop "${applications_dir}/phux-desktop"; return; fi
  fetch_verified
  unpack_linux_desktop
  publish_linux_desktop
  linux_desktop_launcher || phux_message warn "could not install launcher; run ${installed}/bin/phux-desktop"
  register_linux_desktop || phux_message warn "could not register the desktop menu entry; run ${installed}/bin/phux-desktop"
  phux_message ok "installed Phux ${build_id} to ${installed}"
  printf 'next: %s\n' "$(shell_quote "${installed}/bin/phux-desktop")"
}

# --- main --------------------------------------------------------------------

channel="${PHUX_CHANNEL:-stable}"
pin=""
pinned_flag=""
pin_sha256=""
install_dir="${PHUX_INSTALL_DIR:-${HOME:-}/.local/bin}"
applications_dir="${PHUX_DESKTOP_APPLICATIONS_DIR:-}"
stage_dir=""
desktop=0
dry_run=0
os=""
arch=""
modify_path=1
case "${PHUX_NO_MODIFY_PATH:-}" in '' | 0) ;; *) modify_path=0 ;; esac

while [ "$#" -gt 0 ]; do
  case "$1" in
    --channel | --version | --revision | --sha256 | --install-dir | --applications-dir | --stage-dir | --os | --arch)
      [ "$#" -ge 2 ] && [ -n "$2" ] || die "$1 requires a value"
      ;;
  esac
  case "$1" in
    --channel) channel="$2" ;;
    --version)
      pin="${2#desktop-}"
      pin="${pin#v}"
      pinned_flag=--version
      ;;
    --revision) pin="$2" pinned_flag=--revision ;;
    --sha256) pin_sha256="$2" ;;
    --install-dir) install_dir="$2" ;;
    --applications-dir) applications_dir="$2" ;;
    --stage-dir) stage_dir="$2" ;;
    --os) os="$2" ;;
    --arch) arch="$2" ;;
    --desktop) desktop=1 ;;
    --dry-run) dry_run=1 ;;
    --no-modify-path) modify_path=0 ;;
    --help | -h) usage; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
  case "$1" in --desktop | --dry-run | --no-modify-path) shift ;; *) shift 2 ;; esac
done

case "$channel" in
  latest) channel=stable ;;
  stable | next) ;;
  *) die "--channel must be stable, latest, or next" ;;
esac
if [ -n "$pin" ]; then
  if [ "$channel" = next ]; then
    [ "$pinned_flag" = --revision ] && matches "$pin" '[0-9a-f]{40}' \
      || die "on the next channel, pin a build with --revision <40-character commit>"
  else
    [ "$pinned_flag" = --version ] && matches "$pin" "$SEMVER" \
      || die "on the stable channel, pin a release with --version X.Y.Z"
  fi
fi
[ -z "$pin_sha256" ] || { [ -n "$pin" ] && matches "$pin_sha256" '[0-9a-f]{64}'; } \
  || die "--sha256 needs 64 lowercase hex digits and a --version or --revision pin"
[ -n "$install_dir" ] || die "--install-dir resolved to an empty path"
if [ -n "$stage_dir" ]; then
  [ "$desktop" -eq 1 ] || die "--stage-dir stages Phux Desktop; pass --desktop"
  case "$stage_dir" in /?*) stage_dir="${stage_dir%/}" ;; *) die "--stage-dir must be an absolute directory other than /" ;; esac
  # Stage on the volume the app swaps into, under the same lock.
  applications_dir="$(dirname "$stage_dir")"
elif [ -z "$applications_dir" ]; then
  case "${os:-$(uname -s)}" in
    [Ll]inux) applications_dir="$(linux_data_home)" ;;
    *) if [ -w /Applications ]; then applications_dir=/Applications; else applications_dir="${HOME:-}/Applications"; fi ;;
  esac
fi

case "${os:-$(uname -s)}/${arch:-$(uname -m)}" in
  [Dd]arwin/arm64 | [Dd]arwin/aarch64) target=aarch64-apple-darwin ;;
  [Ll]inux/x86_64 | [Ll]inux/amd64) target=x86_64-unknown-linux-gnu ;;
  [Ll]inux/arm64 | [Ll]inux/aarch64) target=aarch64-unknown-linux-gnu ;;
  *) die "no phux release for ${os:-$(uname -s)}/${arch:-$(uname -m)}; build from source" ;;
esac

work="$(mktemp -d)"
cli_locked=0
cli_publishing=0
cli_published=""
cli_publish=""
app_publishing=0
app_published=0
app_publish=""
app_locked=0
app_lock=""
stage_tmp=""
cleanup() {
  rollback_cli
  rollback_desktop || true
  rm -rf "$work" "$cli_publish" "$app_publish" "$stage_tmp"
  [ "$cli_locked" -eq 0 ] || rmdir "${install_dir}/.phux-install.lock" 2>/dev/null || true
  [ "$app_locked" -eq 0 ] || rmdir "$app_lock" 2>/dev/null || true
}
trap cleanup EXIT
trap_signals

if [ "$desktop" -eq 1 ]; then
  install_desktop
else
  install_cli "$pin" "$pin_sha256"
  [ "$dry_run" -eq 1 ] || report_path
fi
