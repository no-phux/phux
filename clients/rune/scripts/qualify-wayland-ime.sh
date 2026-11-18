#!/usr/bin/env bash
# The private D-Bus/Wayland leg of qualify-wayland.sh. Only ASCII keys go in;
# both the real Pinyin preedit/commit and the server's terminal echo must follow.
set -euo pipefail
scratch="${1:?usage: qualify-wayland-ime.sh SCRATCH BINARY PHUX SOCKET SESSION}"
binary="${2:?}"
phux="${3:?}"
socket="${4:?}"
session="${5:?}"
app_pid=""

WAYLAND_DEBUG=1 fcitx5 >"$scratch/wayland-fcitx5.log" 2>&1 &
fcitx_pid=$!
finish() {
    [[ -z "$app_pid" ]] || kill "$app_pid" 2>/dev/null || true
    kill "$fcitx_pid" 2>/dev/null || true
    [[ -z "$app_pid" ]] || wait "$app_pid" 2>/dev/null || true
    wait "$fcitx_pid" 2>/dev/null || true
}
trap finish EXIT

context() {
    dbus-send --session --print-reply --dest=org.fcitx.Fcitx5 /controller \
        org.fcitx.Fcitx.Controller1.DebugInfo >"$scratch/wayland-ime-context.log" 2>&1
}
fail() {
    echo "qualify: wayland IME: $*" >&2
    context || true
    swaymsg -t get_tree >"$scratch/wayland-ime-tree.json" 2>/dev/null || true
    grim "$scratch/wayland-ime-failed.png" 2>/dev/null || true
    exit 1
}
until_true() {
    local what="$1"
    shift
    for _ in $(seq 50); do
        if "$@"; then echo "qualify: wayland IME: $what"; return 0; fi
        kill -0 "$fcitx_pid" 2>/dev/null || fail "fcitx exited while waiting for $what"
        kill -0 "${app_pid:-$fcitx_pid}" 2>/dev/null || fail "the app exited while waiting for $what"
        sleep 0.2
    done
    fail "timed out waiting for $what"
}
bus_ready() {
    dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus \
        org.freedesktop.DBus.NameHasOwner string:org.fcitx.Fcitx5 | grep -q 'boolean true'
}
focused() {
    swaymsg -t get_tree >"$scratch/wayland-ime-tree.json"
    jq -e --argjson pid "$app_pid" \
        '.. | objects | select(.pid? == $pid and .app_id? == "sh.phux.rune" and .focused? == true)' \
        "$scratch/wayland-ime-tree.json" >/dev/null
}
activated() {
    context && grep -q 'program:sh.phux.rune frontend:wayland_v2 .*focus:1' "$scratch/wayland-ime-context.log"
}
event() { grep -Fq -- "$1" "$scratch/wayland-ime-app.log"; }
last_cursor=""
settled() {
    local cursor
    cursor="$(grep -c 'zwp_text_input_v3.*set_cursor_rectangle' "$scratch/wayland-ime-app.log" || true)"
    [[ "$cursor" != 0 && "$cursor" == "$last_cursor" ]] && return 0
    last_cursor="$cursor"
    return 1
}
echoed() {
    "$phux" --socket "$socket" snapshot "$session" >"$scratch/wayland-ime-terminal.log"
    grep -q '^│\$ 你好' "$scratch/wayland-ime-terminal.log"
}

until_true "fcitx owns its D-Bus name" bus_ready
WAYLAND_DEBUG=1 "$binary" --socket "$socket" --session "$session" >"$scratch/wayland-ime-app.log" 2>&1 &
app_pid=$!
until_true "the new Rune window has keyboard focus" focused
until_true "Rune enabled its Wayland input context" activated
# Every cursor update commits a new text-input serial. Do not send a burst
# while initial layout frames are still invalidating the input method's batch.
until_true "the startup IME cursor updates have settled" settled
fcitx5-remote -s pinyin || fail "could not select Pinyin"
fcitx5-remote -o || fail "could not activate Pinyin"
[[ "$(fcitx5-remote -n)" == pinyin ]] || fail "the active engine is not Pinyin"
# Keep one virtual keyboard alive for the whole composition: ending wtype
# between preedit and candidate selection retires its keymap and grab. Keep
# its existing key pacing; readiness above and the assertions below are not
# inferred from these delays. Control, not Shift, avoids switching to English.
wtype -k Control_L -s 500 'nihao' -s 800 '1' -s 500 -k Return \
    || fail "the virtual keyboard failed during composition"
until_true "Pinyin preedit reached Rune" event '.preedit_string("ni hao",'
until_true "Pinyin committed 你好 to Rune" event '.commit_string("你好")'
until_true "the server echoed the composed text" echoed
grim "$scratch/wayland-ime.png"
