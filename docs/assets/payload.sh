#!/usr/bin/env bash
# payload.sh — Beat 1 of the product demo (docs/demo.md).
#
# Paints the things a byte-reparsing multiplexer degrades: a truecolor
# gradient, curly underlines, an OSC 8 hyperlink, and a kitty-graphics
# image. Run it in a pane, detach, reattach; all of it comes back.
#
# Self-contained: the image is a PNG embedded below as base64, emitted
# straight through the kitty graphics protocol. No `kitty` binary
# required — only a terminal that renders the protocol (Ghostty, kitty,
# WezTerm).
set -euo pipefail

# Transmit + display a PNG at the cursor, chunked per the kitty graphics
# protocol (4096 bytes of base64 per escape, m=1 continues, m=0 ends).
emit_kitty_png() {
  local b64="$1" first=1 chunk ctrl m
  while [ -n "$b64" ]; do
    chunk=${b64:0:4096}
    b64=${b64:4096}
    [ -n "$b64" ] && m="m=1" || m="m=0"
    if [ "$first" = 1 ]; then
      ctrl="a=T,f=100,$m"  # transmit keys ride only the first chunk
      first=0
    else
      ctrl="$m"
    fi
    # shellcheck disable=SC1003 # the trailing \\ is ESC ST, not a quote escape
    printf '\033_G%s;%s\033\\' "$ctrl" "$chunk"
  done
}

# A 96x96 hue wheel. Regenerate with any PNG and `base64` if you want a
# different image; emit_kitty_png handles arbitrary sizes.
PNG_B64=$(tr -d '\n' <<'PNG'
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
PNG
)

cols=$( (tput cols) 2>/dev/null || echo "${COLUMNS:-80}")

printf '\n'

# Truecolor sweep — every column a distinct 24-bit color. tmux quantizes
# this to its palette; phux carries the bytes.
awk -v n="$cols" 'BEGIN {
  for (j = 0; j < 2; j++) {
    for (i = 0; i < n; i++) {
      c = int(i * 255 / (n - 1))
      printf "\033[48;2;%d;%d;%dm ", c, (128 + c) % 256, 255 - c
    }
    print "\033[0m"
  }
}'
printf '\n'

# Styled text: bold, italic, curly underline (SGR 4:3) with a truecolor
# underline color (SGR 58), strikethrough, and an OSC 8 hyperlink.
printf '\033[1mbold\033[0m  \033[3mitalic\033[0m  '
printf '\033[4:3m\033[58;2;255;80;80mcurly underline\033[0m  '
printf '\033[9mstrikethrough\033[0m  '
printf '\033]8;;https://github.com/phall1/phux\033\\phall1/phux\033]8;;\033\\\n\n'

emit_kitty_png "$PNG_B64"
printf '\n\n'

printf '\033[2mtruecolor / styles / OSC 8 / kitty graphics\033[0m\n'
