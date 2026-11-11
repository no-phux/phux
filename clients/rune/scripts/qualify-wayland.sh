#!/usr/bin/env bash
# Prove the packaged app works on Wayland the way a person uses it: a real
# window on a wlroots compositor (a headless sway), typed into, pasted into,
# copied from and rescaled. qualify.sh runs this on Linux when the tools are
# installed.
#
#   clients/rune/scripts/qualify-wayland.sh BINARY PHUX SOCKET SCRATCH
#
# SOCKET is a running same-source server. Needs sway, wtype, wl-clipboard,
# grim, jq and python3. Leaves wayland-*.png and wayland-*.log in SCRATCH.
set -euo pipefail

binary="${1:?usage: qualify-wayland.sh BINARY PHUX SOCKET SCRATCH}"
phux="${2:?}"
socket="${3:?}"
scratch="${4:?}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
session=rune-wayland
runtime="$scratch/wl-sway"
size=(1280 800)

fail() {
    echo "qualify: wayland: $*" >&2
    exit 1
}

# As qualify.sh: nothing inherited can aim this at a real server or display.
clean=(env -i HOME="$scratch/home" PATH=/usr/bin:/bin SHELL=/bin/sh TERM=dumb TMPDIR="$scratch"
    LANG=C.UTF-8 PHUX_PROFILE=rune-qualify RUNE_PREFS_DIR="$scratch/prefs-wayland"
    XDG_RUNTIME_DIR="$runtime"
    ${VK_DRIVER_FILES:+VK_DRIVER_FILES="$VK_DRIVER_FILES"}
    ${WGPU_BACKEND:+WGPU_BACKEND="$WGPU_BACKEND"})

mkdir -m 700 "$runtime"
mkdir -p "$scratch/prefs-wayland"
printf 'output HEADLESS-1 resolution %sx%s scale 1\n' "${size[@]}" >"$scratch/sway.conf"
"${clean[@]}" WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
    sway -c "$scratch/sway.conf" >"$scratch/wayland-sway.log" 2>&1 &
sway_pid=$!
app_pid=""
finish() {
    [[ -z "$app_pid" ]] || kill "$app_pid" 2>/dev/null || true
    kill "$sway_pid" 2>/dev/null || true
    wait "$sway_pid" 2>/dev/null || true
}
trap finish EXIT

display=""
for _ in $(seq 100); do
    display="$(find "$runtime" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' -printf '%f\n' -quit)"
    [[ -z "$display" ]] || break
    sleep 0.1
done
[[ -n "$display" ]] || fail "sway never listened: $(cat "$scratch/wayland-sway.log")"
ipc="$(find "$runtime" -maxdepth 1 -name 'sway-ipc.*.sock' -print -quit)"
wl=("${clean[@]}" WAYLAND_DISPLAY="$display" SWAYSOCK="$ipc")

# A UTF-8 shell: in the C locale readline eats the bytes of non-ASCII text.
"${clean[@]}" "$phux" --socket "$socket" new --json -s "$session" -- \
    /usr/bin/env LANG=C.UTF-8 PS1='$ ' bash --norc >/dev/null
"${wl[@]}" "$binary" --socket "$socket" --session "$session" >"$scratch/wayland-app.log" 2>&1 &
app_pid=$!

screen() { "${clean[@]}" "$phux" --socket "$socket" snapshot "$session" 2>/dev/null || true; }
window() {
    "${wl[@]}" swaymsg -t get_tree \
        | jq -r '.. | objects | select(.app_id? == "sh.phux.rune") | "\(.rect.width)x\(.rect.height)"'
}
# Poll for up to ten seconds: a hosted runner draws slowly.
until_true() {
    local what="$1"
    shift
    for _ in $(seq 50); do
        if "$@"; then return 0; fi
        kill -0 "$app_pid" 2>/dev/null || fail "the app exited: $(cat "$scratch/wayland-app.log")"
        sleep 0.2
    done
    "${wl[@]}" grim "$scratch/wayland-failed.png" 2>/dev/null || true
    fail "$what"
}
# Into a variable first: under pipefail a grep that stops reading early
# fails the pipe it is in.
shows() {
    local text
    text="$(screen)"
    grep -q -- "$1" <<<"$text"
}
# wtype swaps in its own keymap for each run, and keys sent before a client
# has taken the new keymap are lost or misread (other terminals lose them
# too). So a throwaway Shift makes the switch and a pause follows it. Its
# keycodes are not a real keyboard's either, so chords here use named keys,
# which the app matches by name rather than by position.
keys() {
    "${wl[@]}" wtype -k Shift_L -s 400 "$@"
    sleep 0.3
}
pointer() { "${wl[@]}" python3 "$here/wayland_pointer.py" "$@"; }
cols() { screen | sed -n 's/.*pane=[0-9]* \([0-9]*\)x[0-9]*.*/\1/p' | tail -1; }

has_window() { [[ -n "$(window)" ]]; }
until_true "the window never opened on Wayland" has_window
until_true "the session's prompt never drew" shows '^│\$ '
sleep 1

# Typing, and text no ASCII keyboard has.
# shellcheck disable=SC2016  # the pane's shell does the arithmetic, not this one
keys 'echo wl-typed-$((40+2))' -k Return
until_true "typed text never reached the terminal" shows '^│wl-typed-42'
keys 'echo é ü ñ 日本語 ✓' -k Return
until_true "non-ASCII text did not arrive whole" shows '^│é ü ñ 日本語 ✓'

# Paste from the Wayland clipboard (Shift+Insert).
printf 'PASTED-%s' 123 | "${wl[@]}" wl-copy 2>/dev/null
keys 'echo '
keys -M shift -k Insert -m shift
keys -k Return
until_true "paste from the Wayland clipboard did not arrive" shows '^│PASTED-123'

# Select with the pointer, copy to the Wayland clipboard (Ctrl+Insert).
keys 'clear' -k Return
keys 'echo COPY-FROM-RUNE' -k Return
until_true "the copy line never drew" shows '^│COPY-FROM-RUNE'
printf 'stale' | "${wl[@]}" wl-copy 2>/dev/null
# A drag from the first row down across the pane: it starts wide of the
# sidebar and below the header whatever their exact sizes are, and takes the
# whole second row with it.
pointer "${size[@]}" at 640 400 at 420 72 down at 600 120 at 900 200 at 1150 330 up wait 0.4
keys -M ctrl -k Insert -m ctrl
copied() {
    local text
    text="$("${wl[@]}" wl-paste -n 2>/dev/null || true)"
    [[ "$text" == *COPY-FROM-RUNE* ]]
}
until_true "a selection did not reach the Wayland clipboard" copied
keys 'clear' -k Return

# Fractional scaling: the window follows the output and keeps working.
before="$(cols)"
"${wl[@]}" grim "$scratch/wayland-scale-1.png"
for scale in 1.5 1.25 1; do
    "${wl[@]}" swaymsg -q output HEADLESS-1 scale "$scale"
    if [[ "$scale" != 1 ]]; then
        narrower() { local now; now="$(cols)"; [[ -n "$now" && "$now" -lt "$before" ]]; }
        until_true "the terminal did not resize at scale $scale" narrower
    else
        restored() { [[ "$(cols)" == "$before" ]]; }
        until_true "the terminal did not return to $before columns at scale 1" restored
    fi
    keys "echo scale-$scale" -k Return
    until_true "typing stopped working at scale $scale" shows "^│scale-$scale"
    "${wl[@]}" grim "$scratch/wayland-scale-$scale.png"
done
echo "qualify: wayland: window, typing, clipboard and scaling hold on sway $(sway --version | awk '{print $3}')"
