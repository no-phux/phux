
# --- Phux Desktop ------------------------------------------------------------

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

check_macos_desktop_host() {
  [ "$(uname -s)" = Darwin ] && [ "$(uname -m)" = arm64 ] || die "Phux Desktop requires Apple silicon macOS or GNU/Linux"
  macos_at_least "$(sw_vers -productVersion)" 26.0 || die "Phux Desktop requires macOS 26 or later"
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
  check_macos_desktop_bundle
  check_macos_desktop_release
}

check_macos_desktop_bundle() {
  [ -x "${app}/Contents/MacOS/phux-desktop" ] || die "the archive has no desktop executable"
  codesign --verify --deep --strict "$app" || die "the desktop app's signature does not verify"
  [ "$(plist "$app" CFBundleIdentifier)" = dev.phux.desktop ] || die "the archive is not Phux Desktop"
  minimum_macos="$(plist "$app" LSMinimumSystemVersion)" || die "the bundle has no minimum macOS version"
  macos_at_least "$(sw_vers -productVersion)" "$minimum_macos" || die "this desktop bundle requires macOS ${minimum_macos}"
}

check_macos_desktop_release() {
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

desktop_matches_install() {
  [ -x "$installed/Contents/MacOS/phux-desktop" ] || return 1
  diff -qr "$app" "$installed" >/dev/null 2>&1
}

publish_desktop() {
  installed="${applications_dir}/Phux.app"
  if [ -e "$installed" ] && [ ! -d "$installed" ]; then
    die "refusing to replace non-directory install destination: ${installed}"
  fi
  if [ -d "$installed" ] && [ "$(plist "$installed" CFBundleIdentifier || true)" != dev.phux.desktop ]; then
    die "refusing to replace an unrelated ${installed}"
  fi
  # Only byte-identical verified bundles are a no-op: a matching version alone
  # cannot detect a corrupted install or a different next-channel build.
  if desktop_matches_install; then
    xattr -d com.apple.quarantine "$installed" 2>/dev/null || true
    return
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
  if cmp -s "$launcher" "${install_dir}/phux-desktop" && [ -x "${install_dir}/phux-desktop" ]; then
    rm -f "$launcher"
    return
  fi
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
    check_macos_desktop_host
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
    phux-desktop/share/phux-desktop/Inter-OFL.txt | \
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
