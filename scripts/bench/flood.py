#!/usr/bin/env python3
"""TUI-style redraw flood: repaint the whole screen N times per second.

Only digits, punctuation and box drawing are emitted so an echo probe in a
sibling pane can never mistake flood output for its typed letter.
"""
import os, sys, time, shutil

fps = float(sys.argv[1]) if len(sys.argv) > 1 else 30.0
mode = sys.argv[2] if len(sys.argv) > 2 else "full"   # full | spinner
cols, rows = shutil.get_terminal_size((80, 24))
out = sys.stdout.buffer
palette = [31, 32, 33, 34, 35, 36, 91, 92, 93, 94, 95, 96]
frame = 0
os.write(1, b"\x1b[?25l\x1b[2J")
period = 1.0 / fps
while True:
    t0 = time.monotonic()
    buf = bytearray()
    if mode == "full":
        for r in range(1, rows):
            buf += b"\x1b[%d;1H" % r
            for c in range(0, cols, 8):
                color = palette[(r + c // 8 + frame) % len(palette)]
                buf += b"\x1b[%dm%08d" % (color, (frame * 7919 + r * 131 + c) % 100000000)
        buf += b"\x1b[0m\x1b[%d;1H\x1b[2K frame %d " % (rows, frame)
    else:
        spin = "|/-\\"[frame % 4].encode()
        buf += b"\x1b[%d;1H\x1b[2K\x1b[33m%s\x1b[0m %d" % (rows, spin, frame)
    out.write(buf)
    out.flush()
    frame += 1
    dt = time.monotonic() - t0
    if dt < period:
        time.sleep(period - dt)
