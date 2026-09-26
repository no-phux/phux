"""Deterministic VT producer running in the real server-owned PTY."""
import os
import tty

tty.setraw(0)


def write(text):
    os.write(1, text.encode())


def draw():
    write("\x1b[?2004h\x1b[?25l\x1b[0m\x1b[2J\x1b[H")
    for row in range(50):
        write(f"history {row:02d} one PTY, independent views\r\n")
    write("\x1b[2J\x1b[HGPUI NATIVE TERMINAL")
    write("\x1b[2;1HA界e\u0301🙂Z")
    write("\x1b[3;1H\x1b[38;2;240;60;30;48;2;20;40;160mRGB")
    write("\x1b[7mINV\x1b[0m \x1b[1;3mBOLDITALIC\x1b[0m")
    write("\x1b[4;1H\x1b]8;;https://example.test/native\x1b\\LINK\x1b]8;;\x1b\\")
    write("\x1b[5;1H┌───┬───┐  λ ∑ → ┃\x1b[6;1H└───┴───┘")
    write("\x1b[7;1H\x1b[4munder\x1b[0m \x1b[4:2mdouble\x1b[0m ")
    write("\x1b[4:3mcurly\x1b[0m \x1b[4:4mdots\x1b[0m \x1b[4:5mdash\x1b[0m")
    write("\x1b[8;1H\x1b[9mstrike\x1b[0m \x1b[53mover\x1b[0m \x1b[2mfaint\x1b[0m \x1b[8mSECRET\x1b[0m")
    for row in range(8, 14):
        write(f"\x1b[{row + 1};1H")
        for col in range(60):
            write(f"\x1b[38;2;{col * 4};{row * 17};170;48;2;{row * 7};20;{col * 3}m{chr(65 + col % 26)}")
    write("\x1b[0m\x1b[16;1HREADY\x1b[16;10H\x1b[2 q\x1b[?25h")


def pixels():
    write("\x1b[?25l\x1b[0m\x1b[2J\x1b[H🙂\x1b[1;5H\x1b[2m🙂\x1b[0m")
    write("\x1b[2;1HM\x1b[2;5H\x1b[2mM\x1b[0m")
    write("\x1b[3;1H\x1b[38;2;240;60;30;48;2;20;40;160mM")
    write("\x1b[3;5H\x1b[7mM\x1b[27m\x1b[3;9H \x1b[0m")
    for col, style in ((4, "4"), (8, "4:2"), (12, "9"), (16, "53"),
                       (20, "4:3"), (24, "4:4"), (28, "4:5")):
        write(f"\x1b[4;{col + 1}H\x1b[{style}m \x1b[0m")
    write("\x1b[6;1H界\x1b[6;5H界\x1b[6;7HM")
    write("\x1b[7;1He\u0301\x1b[7;5Hé\x1b[7;9Hλ\x1b[7;13H∑")
    write("\x1b[16;1HPIXELREADY\x1b[5;5H\x1b[2 q\x1b[?25h")


write("\x1b[?2004hPTY awaiting DRAW")
pending = b""
sequences = {
    b"UPDATE": "\x1b[1;1HUPDATED FRAME\x1b[16;10H",
    b"ALT": "\x1b[?1049h\x1b[2J\x1b[HALTERNATE SCREEN\x1b[?25l",
    b"MAIN": "\x1b[?1049l\x1b[?25h",
    b"CURSORBAR": "\x1b[6 q",
    b"CURSORUNDER": "\x1b[4 q",
}
while True:
    pending += os.read(0, 4096)
    for command in (b"DRAW", b"PIXELS", *sequences):
        if command not in pending:
            continue
        pending = pending.split(command, 1)[1]
        if command == b"DRAW":
            draw()
        elif command == b"PIXELS":
            pixels()
        else:
            write(sequences[command])
