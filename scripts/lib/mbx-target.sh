#!/usr/bin/env bash
# Heal a worktree `target` symlink after mbx garbage-collects its cache
# directory. Cargo then fails with "Not a directory" (os error 20) until
# the destination exists again. Only an mbx-managed path is recreated.
# Any other symlink is left alone.

mbx_heal_target() {
    local root="${1:-$PWD}"
    local link="$root/target"
    local dest
    [[ -L "$link" ]] || return 0
    dest="$(readlink -- "$link" 2>/dev/null || readlink "$link")" || return 0
    [[ -n "$dest" && ! -e "$dest" ]] || return 0
    case "$dest" in
        */mbx/targets/*) ;;
        *) return 0 ;;
    esac
    mkdir -p -- "$dest"
}
