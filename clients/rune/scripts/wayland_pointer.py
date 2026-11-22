#!/usr/bin/env python3
"""A virtual pointer for a wlroots compositor, over the raw Wayland wire.

    wlptr.py WIDTH HEIGHT  at X Y | down | up | click | wait SECONDS ...

Stands in for a mouse where no tool that speaks wlr-virtual-pointer is
packaged. Coordinates are in the output's logical pixels.
"""
import os, socket, struct, sys, time

sock = socket.socket(socket.AF_UNIX)
sock.connect(os.path.join(os.environ["XDG_RUNTIME_DIR"], os.environ["WAYLAND_DISPLAY"]))
next_id = [2]

def new_id():
    next_id[0] += 1
    return next_id[0] - 1

def send(obj, opcode, payload=b""):
    sock.sendall(struct.pack("<IHH", obj, opcode, 8 + len(payload)) + payload)

def string(text):
    raw = text.encode() + b"\0"
    return struct.pack("<I", len(raw)) + raw + b"\0" * (-len(raw) % 4)

def roundtrip(on_event=None):
    """wl_display.sync, then read until its callback fires."""
    callback = new_id()
    send(1, 0, struct.pack("<I", callback))
    buf = b""
    while True:
        while len(buf) >= 8:
            obj, opcode, size = struct.unpack("<IHH", buf[:8])
            if len(buf) < size:
                break
            body, buf = buf[8:size], buf[size:]
            if obj == callback:
                return
            if on_event:
                on_event(obj, opcode, body)
        chunk = sock.recv(65536)
        if not chunk:
            sys.exit("the compositor closed the connection")
        buf += chunk

registry = new_id()
send(1, 1, struct.pack("<I", registry))
found = {}

def on_global(obj, opcode, body):
    if obj == registry and opcode == 0:
        name, length = struct.unpack("<II", body[:8])
        interface = body[8:8 + length - 1].decode()
        version = struct.unpack("<I", body[8 + length + (-length % 4):][:4])[0]
        found[interface] = (name, version)

roundtrip(on_global)
if "zwlr_virtual_pointer_manager_v1" not in found:
    sys.exit("this compositor has no wlr-virtual-pointer")
name, _ = found["zwlr_virtual_pointer_manager_v1"]
manager = new_id()
send(registry, 0, struct.pack("<I", name) + string("zwlr_virtual_pointer_manager_v1")
     + struct.pack("<II", 1, manager))
pointer = new_id()
send(manager, 0, struct.pack("<II", 0, pointer))
roundtrip()

width, height = int(sys.argv[1]), int(sys.argv[2])
now = lambda: int(time.monotonic() * 1000) & 0xFFFFFFFF
LEFT = 0x110

def button(state):
    send(pointer, 2, struct.pack("<III", now(), LEFT, state))
    send(pointer, 4)
    roundtrip()

args = sys.argv[3:]
while args:
    word = args.pop(0)
    if word == "at":
        x, y = int(args.pop(0)), int(args.pop(0))
        send(pointer, 1, struct.pack("<IIIII", now(), x, y, width, height))
        send(pointer, 4)
        roundtrip()
    elif word == "down":
        button(1)
    elif word == "up":
        button(0)
    elif word == "click":
        button(1)
        time.sleep(0.03)
        button(0)
    elif word == "wait":
        time.sleep(float(args.pop(0)))
    else:
        sys.exit(f"unknown word {word}")
    time.sleep(0.05)
