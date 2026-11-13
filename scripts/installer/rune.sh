
# --- Phux Rune ---------------------------------------------------------------

plist() {
  /usr/libexec/PlistBuddy -c "Print :$2" "$1/Contents/Info.plist" 2>/dev/null
}

macos_at_least() {
  awk -v actual="$1" -v minimum="$2" 'BEGIN {
    pattern = "^[0-9]+([.][0-9]+){0,2}$"
    if (actual !~ pattern || minimum !~ pattern) exit 1
    split(actual, a, "."); split(minimum, b, ".")
    for (i = 1; i <= 3; i++) {
      if (a[i]+0 > b[i]+0) exit 0
      if (a[i]+0 < b[i]+0) exit 1
    }
  }'
}

lock_applications() {
  mkdir -p "$applications_dir"
  exec 9>"${applications_dir}/.phux-desktop-install.lock"
  lockf -s -t 0 9 || die "another install is publishing to ${applications_dir}"
}

rollback_app() {
  [ "$app_publishing" -eq 1 ] || return 0
  [ "$app_published" -eq 0 ] || rm -rf "$installed"
  [ ! -d "${app_publish}/previous.app" ] || mv -f "${app_publish}/previous.app" "$installed" || {
    phux_message error "rollback failed; the previous app is at ${app_publish}/previous.app"
    app_publish=""
  }
}

has_cli() {
  for candidate in "${install_dir}/phux" "${HOME:-}/.local/bin/phux" "${HOME:-}/.cargo/bin/phux" /opt/homebrew/bin/phux /usr/local/bin/phux /usr/bin/phux; do
    [ ! -x "$candidate" ] || return 0
  done
  command -v phux >/dev/null 2>&1
}

linux_data_home() {
  case "${XDG_DATA_HOME:-}" in
    /*) printf '%s\n' "$XDG_DATA_HOME" ;;
    *) printf '%s\n' "${HOME:-}/.local/share" ;;
  esac
}

register_desktop_entry() {
  desktop_source="$1"
  [ -f "$desktop_source" ] || return 1
  bundle_path="$(cd "$installed" && pwd -P)" || return 1
  case "$bundle_path" in *'
'*) return 1 ;; esac
  executable="$(printf '%s' "${bundle_path}/$3" | sed 's/%/%%/g; s/[\\"`$]/\\&/g; s/\\/\\\\/g')"
  icon="$(printf '%s' "${bundle_path}/$4" | sed 's/\\/\\\\/g')"
  entries="$(linux_data_home)/applications"
  mkdir -p "$entries" || return 1
  [ ! -d "${entries}/$2.desktop" ] || return 1
  entry="$(mktemp "${entries}/.phux-desktop.XXXXXX")" || return 1
  # The executable is replaced; the field codes after it (`%u`) stay.
  if PHUX_DESKTOP_EXEC="\"${executable}\"" PHUX_DESKTOP_ICON="$icon" awk '
    /^Exec=/ { codes = $0; sub(/^Exec=[^ ]*/, "", codes); print "Exec=" ENVIRON["PHUX_DESKTOP_EXEC"] codes; next }
    /^Icon=/ { print "Icon=" ENVIRON["PHUX_DESKTOP_ICON"]; next }
    { print }
  ' "$desktop_source" > "$entry" && chmod 644 "$entry" && mv -f "$entry" "${entries}/$2.desktop"; then
    register_scheme_handler "$entries" "$2.desktop"
    return 0
  fi
  rm -f "$entry"
  return 1
}

# phux://connect links open the app: the entry's MimeType names the scheme,
# and the desktop's own tools make it the handler. Without them a desktop
# still reads the entry when it next scans; nothing here is fatal.
register_scheme_handler() {
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$1" >/dev/null 2>&1 || true
  fi
  if command -v xdg-mime >/dev/null 2>&1; then
    xdg-mime default "$2" x-scheme-handler/phux >/dev/null 2>&1 \
      || phux_message warn "could not make Phux Rune open phux:// links; run: xdg-mime default $2 x-scheme-handler/phux"
  fi
}

# Launch Services learns a bundle's schemes when it is first opened; register
# it now so a phux://connect link opens it before then.
register_bundle_macos() {
  lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
  [ ! -x "$lsregister" ] || "$lsregister" -f "$1" >/dev/null 2>&1 || true
}

# The update feed names one version, with a digest per target. Sets build_id,
# build_sha256 and the asset names; versions keep their -alpha.N here.
resolve_rune() {
  [ -s "${work}/rune" ] || fetch_manifest rune "${work}/rune" \
    || die "the Phux Rune feed is malformed"
  [ "$(manifest_value "${work}/rune" schema)" = 1 ] \
    || die "the Phux Rune feed has an unsupported schema; update this installer"
  build_id="$(manifest_value "${work}/rune" version)"
  matches "$build_id" "${SEMVER}(-alpha\\.(0|[1-9][0-9]*))?" || die "the feed names an invalid Phux Rune version"
  build_sha256="$(manifest_value "${work}/rune" "targets.${target}.sha256")"
  matches "$build_sha256" '[0-9a-f]{64}' || die "the feed publishes no Phux Rune digest for ${target}"
  case "$target" in
    aarch64-apple-darwin) asset_name="phux-rune-${build_id}-macos-arm64.zip" ;;
    aarch64-unknown-linux-gnu) asset_name="phux-rune-${build_id}-linux-aarch64.tar.gz" ;;
    *) asset_name="phux-rune-${build_id}-linux-x86_64.tar.gz" ;;
  esac
  asset_url="${RELEASES}/rune-v${build_id}/${asset_name}"
  sidecar_url="${asset_url}.sha256"
}

unpack_rune_macos() {
  unzip -Z1 "${work}/${asset_name}" > "${work}/members" || die "could not read the archive"
  unzip -Z -l "${work}/${asset_name}" | awk '$1 ~ /^l/ { found = 1 } END { exit found }' \
    || die "the archive contains symlinks"
  while IFS= read -r member; do
    case "$member" in
      */../* | */..) die "unsafe archive member path: $member" ;;
      "Phux Rune.app/"*) ;;
      *) die "unexpected archive member: $member" ;;
    esac
  done < "${work}/members"
  mkdir "${work}/rune-app"
  unzip -q "${work}/${asset_name}" -d "${work}/rune-app"
  app="${work}/rune-app/Phux Rune.app"
  [ -x "${app}/Contents/MacOS/phux-rune" ] || die "the archive has no Phux Rune executable"
  codesign --verify --deep --strict "$app" || die "the Phux Rune signature does not verify"
  [ "$(plist "$app" CFBundleIdentifier)" = sh.phux.rune ] || die "the archive is not Phux Rune"
  [ "$(plist "$app" CFBundleShortVersionString)" = "$build_id" ] || die "the bundle is not version ${build_id}"
  minimum_macos="$(plist "$app" LSMinimumSystemVersion)" || die "the bundle has no minimum macOS version"
  macos_at_least "$(sw_vers -productVersion)" "$minimum_macos" || die "Phux Rune requires macOS ${minimum_macos}"
}

publish_rune_macos() {
  installed="${applications_dir}/Phux Rune.app"
  if [ -e "$installed" ] && [ "$(plist "$installed" CFBundleIdentifier || true)" != sh.phux.rune ]; then
    die "refusing to replace an unrelated ${installed}"
  fi
  app_publish="$(mktemp -d "${applications_dir}/.phux-desktop-install.XXXXXX")"
  ditto "$app" "${app_publish}/Phux Rune.app"
  app_publishing=1
  if [ -e "$installed" ]; then mv "$installed" "${app_publish}/previous.app"; fi
  app_published=1
  mv "${app_publish}/Phux Rune.app" "$installed"
  app_publishing=0
  xattr -d com.apple.quarantine "$installed" 2>/dev/null || true
}

install_rune_macos() {
  for tool in unzip ditto codesign lockf; do
    command -v "$tool" >/dev/null 2>&1 || die "$tool is required to install Phux Rune"
  done
  fetch_verified
  unpack_rune_macos
  lock_applications
  publish_rune_macos
  register_bundle_macos "$installed"
  phux_message ok "installed Phux Rune ${build_id} to ${applications_dir}"
  printf 'next: open %s\n' "$(shell_quote "$(cd "$applications_dir" && pwd -P)/Phux Rune.app")"
}

rune_member() {
  case "${1#*/}" in
    phux-rune | phux-rune.release | README.md | LICENSE | NOTICE | THIRD-PARTY-NOTICES.txt | \
    Inter-OFL.txt | JetBrainsMono-OFL.txt | \
    sh.phux.rune.desktop | sh.phux.rune.png) return 0 ;;
    *) return 1 ;;
  esac
}

# Regular files under one directory, checked before anything is extracted: a
# link must never redirect a later member.
unpack_rune_linux() {
  stem="${asset_name%.tar.gz}"
  tar -tzf "${work}/${asset_name}" > "${work}/members" || die "could not read the archive"
  tar -tvzf "${work}/${asset_name}" > "${work}/types" || die "could not read archive types"
  LC_ALL=C awk 'substr($0,1,1) !~ /[-d]/ { bad=1 } END { exit bad }' "${work}/types" \
    || die "Phux Rune archive members must be regular files or directories, not links"
  while IFS= read -r member; do
    case "$member" in
      "$stem" | "$stem/") continue ;;
      "$stem"/*) rune_member "$member" && continue ;;
    esac
    die "unexpected archive member: $member"
  done < "${work}/members"
  mkdir "${work}/rune-app"
  tar -xzf "${work}/${asset_name}" -C "${work}/rune-app" --no-same-owner
  app="${work}/rune-app/${stem}"
  [ -x "${app}/phux-rune" ] || die "the archive has no Phux Rune executable"
  [ -f "${app}/phux-rune.release" ] || die "the archive is not a Phux Rune release"
  [ "$("${app}/phux-rune" --version 2>/dev/null)" = "phux-rune ${build_id}" ] \
    || die "the binary does not run here, or is not version ${build_id}; nothing was installed"
}

publish_rune_linux() {
  mkdir -p "$applications_dir"
  app_lock="${applications_dir}/.phux-desktop-install.lock"
  trap '' HUP INT TERM
  mkdir "$app_lock" 2>/dev/null || die "another install is publishing; if none is running, remove ${app_lock}"
  app_locked=1
  trap_signals
  installed="${applications_dir}/phux-rune"
  [ ! -L "$installed" ] || die "refusing to replace a symlink: ${installed}"
  if [ -e "$installed" ] && [ ! -f "${installed}/phux-rune.release" ]; then
    die "refusing to replace an unrelated ${installed}"
  fi
  app_publish="$(mktemp -d "${applications_dir}/.phux-desktop-install.XXXXXX")"
  cp -R "$app" "${app_publish}/phux-rune"
  app_publishing=1
  if [ -e "$installed" ]; then mv "$installed" "${app_publish}/previous.app"; fi
  app_published=1
  mv "${app_publish}/phux-rune" "$installed"
  app_publishing=0
}

install_rune_linux() {
  fetch_verified
  unpack_rune_linux
  publish_rune_linux
  bundle="$(cd "$installed" && pwd -P)"
  if ! { mkdir -p "$install_dir" && [ ! -d "${install_dir}/phux-rune" ] \
    && ln -sf "${bundle}/phux-rune" "${install_dir}/phux-rune"; }; then
    phux_message warn "could not install the phux-rune launcher; run ${bundle}/phux-rune"
  fi
  register_desktop_entry "${installed}/sh.phux.rune.desktop" sh.phux.rune phux-rune sh.phux.rune.png \
    || phux_message warn "could not register the menu entry; run ${bundle}/phux-rune"
  phux_message ok "installed Phux Rune ${build_id} to ${installed}"
  printf 'next: %s\n' "$(shell_quote "${bundle}/phux-rune")"
}

# On its own feed: there is no channel to pick and nothing to pin, and the
# app updates itself from here on.
install_rune() {
  case "$target" in
    aarch64-apple-darwin | x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu) ;;
    *) die "no Phux Rune build for ${target}" ;;
  esac
  [ "$target" != aarch64-apple-darwin ] || [ "$dry_run" -eq 1 ] || [ "$(uname -s)" = Darwin ] \
    || die "the macOS build of Phux Rune installs only on macOS"
  if ! has_cli; then
    phux_message info "phux CLI not found; installing it first"
    install_cli "" ""
  fi
  resolve_rune
  if [ "$dry_run" -eq 1 ]; then
    print_plan rune "$applications_dir"
    return
  fi
  if [ "$target" = aarch64-apple-darwin ]; then
    install_rune_macos
  else
    install_rune_linux
  fi
}
