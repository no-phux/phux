#!/usr/bin/env bash
# Film the themes demo headless: a dev server on a temp socket, a scratch
# HOME with a Ghostty config to import, and the window driven by
# demo/themes.json into one video. Nothing of this machine's phux is in reach.
#
#   clients/rune/scripts/demo.sh PHUX [OUT_DIR]
#
# PHUX is a dev build (target/debug/phux). Needs ffmpeg. Writes
# OUT_DIR/themes.mp4 and a themes.png poster (default: clients/rune/demo).
set -euo pipefail

phux="${1:?usage: demo.sh PHUX [OUT_DIR]}"
phux="$(cd "$(dirname "$phux")" && pwd)/$(basename "$phux")"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo="$(cd "$here/../.." && pwd)"
out="${2:-$here/demo}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
binary="$here/target/debug/phux-rune"
[[ -x "$binary" ]] || (cd "$here" && cargo build --locked)
command -v ffmpeg >/dev/null || { echo "demo: needs ffmpeg" >&2; exit 1; }

# Under /tmp: a Unix socket path has to stay short.
scratch="$(mktemp -d /tmp/rune-demo.XXXXXX)"
server=""
cleanup() {
    [[ -z "$server" ]] || { kill "$server" 2>/dev/null || true; wait "$server" 2>/dev/null || true; }
    rm -rf "$scratch"
}
trap cleanup EXIT

mkdir -p "$scratch/home/.config/ghostty" "$scratch/prefs"
# The config the Ghostty tile imports: Kanagawa Dragon's colours.
cat >"$scratch/home/.config/ghostty/config" <<'EOF'
background = #181616
foreground = #c5c9c5
cursor-color = #c5c9c5
palette = 0=#0d0c0c
palette = 1=#c4746e
palette = 2=#8a9a7b
palette = 3=#c4b28a
palette = 4=#8ba4b0
palette = 5=#a292a3
palette = 6=#8ea4a2
palette = 7=#c8c093
palette = 8=#a6a69c
palette = 9=#e46876
palette = 10=#87a987
palette = 11=#e6c384
palette = 12=#7fb4ca
palette = 13=#938aa9
palette = 14=#7aa89f
palette = 15=#c5c9c5
EOF

# The shell the demo types into: a short prompt and one command that shows
# the repo's log and the sixteen colours.
cat >"$scratch/home/.zshrc" <<'EOF'
PROMPT='%F{blue}phux%f %F{magenta}❯%f '
showcase() {
    cd "$PHUX_DEMO_REPO" && clear
    git log --oneline --color=always -8
    printf '\n'
    for i in 1 2 3 4 5 6; do printf "\e[3${i}m██████\e[9${i}m██████  "; done
    printf '\e[0m\n\n'
}
EOF

clean=(env -i HOME="$scratch/home" PATH=/usr/bin:/bin:/opt/homebrew/bin SHELL=/bin/zsh TERM=xterm-256color
    TMPDIR="$scratch" PHUX_PROFILE=rune-demo RUNE_PREFS_DIR="$scratch/prefs" PHUX_DEMO_REPO="$repo"
    ${VK_DRIVER_FILES:+VK_DRIVER_FILES="$VK_DRIVER_FILES"} ${WGPU_BACKEND:+WGPU_BACKEND="$WGPU_BACKEND"})

socket="$scratch/s"
"${clean[@]}" "$phux" --socket "$socket" server --session demo >"$scratch/server.log" 2>&1 &
server=$!
for _ in $(seq 1 150); do
    [[ ! -S "$socket" ]] || break
    kill -0 "$server" 2>/dev/null || { echo "demo: the server exited: $(cat "$scratch/server.log")" >&2; exit 1; }
    sleep 0.1
done
[[ -S "$socket" ]] || { echo "demo: the server never listened" >&2; exit 1; }

"${clean[@]}" "$binary" --socket "$socket" --session demo \
    --script "$here/demo/themes.json" --size 1280x800 --scale 2 \
    --keys bottom-right --pointer --duration auto --video "$out/themes.mp4"
"${clean[@]}" "$binary" --socket "$socket" --session demo \
    --script "$here/demo/themes.json" --size 1280x800 --scale 2 \
    --at mark:settings+1.5 --snap "$out/themes.png"
echo "demo: $out/themes.mp4"
