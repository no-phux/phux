#!/usr/bin/env python3
"""Verify unmodified official Paper Mono assets, source copies and bundle copies."""
import argparse
import hashlib
from pathlib import Path
import struct

ROOT = Path(__file__).resolve().parent.parent
REVISION = "e6eaeceaef02e77e3db997711e07a16378de2bd7"
SHA256 = {
    "PaperMono-Regular.ttf": "130e1a09b64b4f150d34c93a6b7bd52d6c5c3513761d1fbc7be42d68398b8c10",
    "PaperMono-Bold.ttf": "6f7f5849f5e115b538cf2ef40e33bca1bdb4fcf8664553cb4e801aa97332d26c",
    "PaperMono-OFL.txt": "ae8bcaf5a046be7e096b5710a8532cdaa5a299462e182752b805dcfa44370e4c",
}


def verify(directory):
    for name, expected in SHA256.items():
        path = directory / name
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
        if actual != expected:
            raise ValueError(f"{path}: differs from paper-design/paper-mono@{REVISION}")


def verify_nerd_supplementary_group():
    # The shared painter uses this pinned font's single supplementary group
    # because the reference font parser only implements BMP cmap4.
    data = (ROOT / "src/fonts/JetBrainsMonoNLNerdFontMono-Regular.ttf").read_bytes()
    tables = {data[at:at + 4]: struct.unpack_from(">II", data, at + 8)
              for at in range(12, 12 + struct.unpack_from(">H", data, 4)[0] * 16, 16)}
    cmap, _ = tables[b"cmap"]
    for index in range(struct.unpack_from(">H", data, cmap + 2)[0]):
        platform, encoding, offset = struct.unpack_from(">HHI", data, cmap + 4 + index * 8)
        if (platform, encoding) != (3, 10):
            continue
        subtable = cmap + offset
        if struct.unpack_from(">H", data, subtable)[0] != 12:
            raise ValueError("Nerd supplementary cmap is no longer format 12")
        groups = [struct.unpack_from(">III", data, subtable + 16 + group * 12)[:2]
                  for group in range(struct.unpack_from(">I", data, subtable + 12)[0])]
        supplementary = [(start, end) for start, end in groups if start >= 0xf0000]
        if supplementary != [(0xf0001, 0xf1af0)]:
            raise ValueError(f"Nerd fallback group changed: {supplementary}")
        return
    raise ValueError("Nerd supplementary cmap is missing")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--resources", type=Path, help="macOS bundle Contents/Resources")
    args = parser.parse_args()
    verify(ROOT / "assets/fonts")
    for name, expected in SHA256.items():
        if name.endswith(".ttf"):
            path = ROOT / "src/fonts" / name
            if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
                raise ValueError(f"{path}: embedded bytes differ from official Paper Mono")
    verify_nerd_supplementary_group()
    if args.resources is not None:
        verify(args.resources / "assets/fonts")
        license_path = args.resources / "PaperMono-OFL.txt"
        if hashlib.sha256(license_path.read_bytes()).hexdigest() != SHA256["PaperMono-OFL.txt"]:
            raise ValueError(f"{license_path}: packaged OFL differs from official license")
    print(f"ok: official Paper Mono regular, bold and OFL at {REVISION}")


if __name__ == "__main__":
    main()
