
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
