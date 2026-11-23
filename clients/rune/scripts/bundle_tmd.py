#!/usr/bin/env python3
"""Bundle an immutable, digest-verified tmd release; never run its installer."""
import hashlib
import io
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import tempfile
import sys
import tarfile
import urllib.request

VERSION = "0.2.0"
SOURCE_REV = "3df118740ae4ad25fc928df0ae3832831c985164"
SOURCE_SHA256 = "ce781dee84191be150fe1acc8bf7d865fef5ba52354271d5b82b6f7b0592471a"
MACOS_TARGET = "aarch64-macos.14.0.0"
# Keep these identical to scripts/zig-mirrors/build.zig.zon (tested below).
ARO_URL = "https://deps.files.ghostty.org/aro-d0c8c4d9c55daa7ef6e40cf0f630a5b5e900989b.tar.gz"
ARO_HASH = "aro-0.0.0-JSD1QtuBNwCASyBtNF3pqTl_W3oAJQGEVyFAtrBSE_Pa"
MAX_ARCHIVE_SIZE = 64 * 1024 * 1024
DIGESTS = {
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
    write_payload(payload, f"Archive SHA-256: {digest}", binaries, notices)


def write_payload(payload, provenance, binaries, notices):
    binaries.mkdir(parents=True, exist_ok=True)
    notices.mkdir(parents=True, exist_ok=True)
    (binaries / "tmd").write_bytes(payload["tmd"])
    (binaries / "tmd").chmod(0o755)
    (notices / "tmd-LICENSE.txt").write_bytes(payload["LICENSE"])
    (notices / "tmd-THIRD-PARTY-NOTICES.md").write_bytes(payload["THIRD_PARTY_NOTICES.md"])
    (notices / "tmd-VERSION.txt").write_text(
        f"tmd {VERSION}\nhttps://github.com/jtnk/tmd\n{provenance}\n"
    )


def download(url):
    with urllib.request.urlopen(url, timeout=60) as response:
        return response.read(64 * 1024 * 1024 + 1)


def seed_aro_cache(global_cache):
    if not re.fullmatch(r"https://deps\.files\.ghostty\.org/aro-[0-9a-f]{40}\.tar\.gz", ARO_URL):
        raise ValueError("invalid aro mirror URL")
    if not re.fullmatch(r"aro-0\.0\.0-[A-Za-z0-9_-]{44}", ARO_HASH):
        raise ValueError("invalid aro package hash")
    archive = global_cache / "p" / f"{ARO_HASH}.tar.gz"
    archive.parent.mkdir(parents=True, exist_ok=True)
    # Keep the original archive untrusted in the compressed cache. Zig verifies
    # its canonical package hash before materializing it or configuring a build.
    # A separate `zig build --fetch` would cold-compile Maker in this 120s stage.
    subprocess.run(
        ["curl", "--disable", "--fail", "--silent", "--show-error", "--location",
         "--proto", "=https", "--proto-redir", "=https", "--connect-timeout", "20",
         "--max-time", "120", "--max-filesize", str(MAX_ARCHIVE_SIZE),
         "--output", str(archive), "--", ARO_URL],
        check=True, timeout=120,
    )
    if archive.stat().st_size > MAX_ARCHIVE_SIZE:
        raise ValueError("aro archive exceeds size limit")


def build_macos(data, binaries, notices):
    # Upstream's macOS archive requires 15; Phux still supports 14. Build the
    # immutable release source for our floor, never rewrite a Mach-O load command.
    if hashlib.sha256(data).hexdigest() != SOURCE_SHA256:
        raise ValueError("tmd source checksum mismatch")
    repo = Path(__file__).resolve().parents[3]
    zig_version = json.loads((repo / ".config/zig-toolchain.json").read_text())["version"]
    actual = subprocess.run(["zig", "version"], check=True, capture_output=True, text=True, timeout=10)
    if actual.stdout.strip() != zig_version:
        raise ValueError(f"tmd needs the repository's Zig {zig_version}")
    with tempfile.TemporaryDirectory(prefix="phux-tmd-build-") as temporary:
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
            archive.extractall(temporary, filter="data")
        source = Path(temporary) / f"tmd-{SOURCE_REV}"
        # Avoid translate_c's failing Codeberg fetch using Ghostty's identical
        # aro package. Never seed an unpacked directory: Zig trusts those.
        global_cache = Path(temporary) / "zig-global-cache"
        seed_aro_cache(global_cache)
        env = {**os.environ, "ZIG_GLOBAL_CACHE_DIR": str(global_cache)}
        subprocess.run(
            ["zig", "build", "--release=small", f"-Dtarget={MACOS_TARGET}",
             "--pkg-dir", str(source / "zig-pkg"),
             "--cache-dir", str(Path(temporary) / "zig-cache")],
            cwd=source, env=env, check=True, timeout=600,
        )
        payload = {name: (source / name).read_bytes() for name in ("LICENSE", "THIRD_PARTY_NOTICES.md")}
        payload["tmd"] = (source / "zig-out/bin/tmd").read_bytes()
        provenance = f"Source: {SOURCE_REV}\nSource SHA-256: {SOURCE_SHA256}\nZig: {zig_version}\nTarget: {MACOS_TARGET}"
        write_payload(payload, provenance, binaries, notices)


def main():
    binaries = Path(sys.argv[1]).resolve()
    notices = Path(sys.argv[2])
    if (platform.system(), platform.machine()) == ("Darwin", "arm64"):
        data = download(f"https://github.com/jtnk/tmd/archive/{SOURCE_REV}.tar.gz")
        build_macos(data, binaries, notices)
    else:
        target, digest = DIGESTS[(platform.system(), platform.machine())]
        data = download(f"https://github.com/jtnk/tmd/releases/download/v{VERSION}/tmd-{target}.tar.gz")
        unpack(data, digest, binaries, notices)
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
