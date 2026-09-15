#!/usr/bin/env python3
"""Fail if ring's aarch64 P-256 wrappers branch into dead-stripped padding.

ring 0.17's Apple ARM64 assembly defines `_p256_mul_mont` and
`_p256_sqr_mont` as global wrappers that `bl` file-local helpers
(`__ecp_nistz256_mul_mont`, `__ecp_nistz256_sqr_mont`). Mach-O
`.subsections_via_symbols` plus dead_strip can drop those helpers while
leaving the wrappers' PC-relative `bl` intact. The next instruction at
the target is then `udf #0` (zeros). QUIC TLS 1.3 handshake verification
(`verify_tls13_signature` → webpki → ring ECDSA) then SIGILL on
`phux-remote-tunnel`; Zig's segfault handler aborts the app.

Confirmed 2026-09-15 DiagnosticReports (Cockpit 0.23.3): EXC_BAD_INSTRUCTION
on `_ring_core_*p256_mul_mont` during the handshake. `catch_unwind` cannot
contain SIGILL. The shipping fix is `link_gc_sections = false`.

This inspects a linked Mach-O: the first `bl` in each
`_ring_core_*p256_{mul,sqr}_mont` wrapper must land on a 64-bit `mul`,
which is how both helpers start in `p256-armv8-asm-ios64.S`.
"""

from __future__ import annotations

import argparse
import struct
import subprocess
import sys
from pathlib import Path

BL_MASK = 0xFC000000
BL_OP = 0x94000000
MUL64_MASK = 0xFF000000
MUL64_OP = 0x9B000000
SYMBOLS = (
    "_ring_core_0_17_14__p256_mul_mont",
    "_ring_core_0_17_14__p256_sqr_mont",
)


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def parse_macho_int(value: str) -> int:
    return int(value, 16) if value.lower().startswith("0x") else int(value)


def symbol_address(path: Path, symbol: str) -> int:
    listed = subprocess.check_output(["/usr/bin/nm", str(path)], text=True)
    for line in listed.splitlines():
        parts = line.split()
        if len(parts) >= 3 and parts[-1] == symbol:
            return int(parts[0], 16)
    fail(f"{path}: missing {symbol}")
    raise AssertionError


def text_mapping(path: Path) -> tuple[int, int, int]:
    listed = subprocess.check_output(["/usr/bin/otool", "-l", str(path)], text=True)
    lines = listed.splitlines()
    i = 0
    while i < len(lines):
        if "sectname __text" in lines[i]:
            fields: dict[str, str] = {}
            for extra in lines[i + 1 : i + 12]:
                pieces = extra.split()
                if len(pieces) >= 2:
                    fields[pieces[0]] = pieces[1]
            if fields.get("segname") == "__TEXT":
                return (
                    parse_macho_int(fields["addr"]),
                    parse_macho_int(fields["size"]),
                    parse_macho_int(fields["offset"]),
                )
        i += 1
    fail(f"{path}: no __TEXT,__text section")
    raise AssertionError


def read_word(data: bytes, vmaddr: int, fileoff: int, address: int) -> int:
    offset = fileoff + (address - vmaddr)
    if offset < 0 or offset + 4 > len(data):
        fail(f"address 0x{address:x} is outside __text")
    return struct.unpack_from("<I", data, offset)[0]


def bl_target(pc: int, inst: int) -> int | None:
    if inst & BL_MASK != BL_OP:
        return None
    imm26 = inst & 0x03FFFFFF
    if imm26 & 0x02000000:
        imm26 -= 0x04000000
    return pc + (imm26 << 2)


def first_bl(data: bytes, vmaddr: int, fileoff: int, start: int, symbol: str) -> tuple[int, int]:
    pc = start
    for _ in range(32):
        inst = read_word(data, vmaddr, fileoff, pc)
        target = bl_target(pc, inst)
        if target is not None:
            return pc, target
        pc += 4
    fail(f"{symbol} has no bl in its first 32 instructions")
    raise AssertionError


def check_symbol(path: Path, data: bytes, vmaddr: int, size: int, fileoff: int, symbol: str) -> None:
    address = symbol_address(path, symbol)
    if not (vmaddr <= address < vmaddr + size):
        fail(f"{symbol} at 0x{address:x} is outside __text")
    pc, target = first_bl(data, vmaddr, fileoff, address, symbol)
    landed = read_word(data, vmaddr, fileoff, target)
    if landed & MUL64_MASK != MUL64_OP:
        helper = (
            "__ecp_nistz256_mul_mont"
            if "mul_mont" in symbol
            else "__ecp_nistz256_sqr_mont"
        )
        fail(
            f"{path}: {symbol} bl at 0x{pc:x} lands on 0x{target:x} "
            f"(word 0x{landed:08x}, want 64-bit mul). "
            f"dead_strip dropped ring's local {helper} helper."
        )
    print(f"ok: {symbol} bl 0x{pc:x} -> mul at 0x{target:x}")


def check(path: Path) -> None:
    vmaddr, size, fileoff = text_mapping(path)
    data = path.read_bytes()
    for symbol in SYMBOLS:
        check_symbol(path, data, vmaddr, size, fileoff, symbol)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, help="Mach-O executable to inspect")
    args = parser.parse_args()
    if not args.binary.is_file():
        fail(f"not a file: {args.binary}")
    check(args.binary)


if __name__ == "__main__":
    main()
