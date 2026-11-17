#!/usr/bin/env python3
"""Bundle an immutable, digest-verified tmd release; never run its installer."""
import hashlib
import io
from pathlib import Path
import platform
import subprocess
import sys
import tarfile
import urllib.request

VERSION = "0.2.0"
DIGESTS = {
    ("Darwin", "arm64"): ("aarch64-macos", "f0856bd79525b39b069daab8a7202c770a37ae057f7cc064edde100a835c446a"),
    ("Linux", "aarch64"): ("aarch64-linux", "cd52ae76510b09d1d60af00c42dcf2c70040c0bb0fb7d2e00f8602627013408b"),
    ("Linux", "x86_64"): ("x86_64-linux", "9e807bc5a68274d2d69246aab14fd5d5746809e6ffc0c319a2e0d592e9b0a22b"),
}


def unpack(data, digest, binaries, notices):
    if hashlib.sha256(data).hexdigest() != digest:
        raise ValueError("tmd archive checksum mismatch")
    # Read only these regular files, never extract archive-supplied paths.
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        payload = {}
        for name in ("tmd", "LICENSE", "THIRD_PARTY_NOTICES.md"):
            member = archive.getmember(name)
            if not member.isfile() or member.size > 8 * 1024 * 1024:
                raise ValueError(f"invalid tmd member: {name}")
            payload[name] = archive.extractfile(member).read()
    binaries.mkdir(parents=True, exist_ok=True)
    notices.mkdir(parents=True, exist_ok=True)
    (binaries / "tmd").write_bytes(payload["tmd"])
    (binaries / "tmd").chmod(0o755)
    (notices / "tmd-LICENSE.txt").write_bytes(payload["LICENSE"])
    (notices / "tmd-THIRD-PARTY-NOTICES.md").write_bytes(payload["THIRD_PARTY_NOTICES.md"])
    (notices / "tmd-VERSION.txt").write_text(
        f"tmd {VERSION}\nhttps://github.com/jtnk/tmd\nArchive SHA-256: {digest}\n"
    )


def main():
    target, digest = DIGESTS[(platform.system(), platform.machine())]
    url = f"https://github.com/jtnk/tmd/releases/download/v{VERSION}/tmd-{target}.tar.gz"
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read(8 * 1024 * 1024 + 1)
    binaries = Path(sys.argv[1]).resolve()
    unpack(data, digest, binaries, Path(sys.argv[2]))
    binary = str(binaries / "tmd")
    version = subprocess.run([binary, "--version"], check=True, capture_output=True, text=True, timeout=5)
    if version.stdout.strip() != f"tmd {VERSION}":
        raise ValueError("bundled tmd version mismatch")
    sample = subprocess.run(
        [binary, "--print", "--no-images", "--plain-headings", "--width", "60"],
        input="# Pancakes\n\nWhisk until **smooth**.\n", check=True,
        capture_output=True, text=True, timeout=5,
    )
    if "Whisk until smooth." not in sample.stdout or "**" in sample.stdout:
        raise ValueError("bundled tmd cannot render Markdown")


if __name__ == "__main__":
    main()
