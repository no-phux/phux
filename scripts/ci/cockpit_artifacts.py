#!/usr/bin/env python3
"""Exact-input cache for the two Cockpit Rust artifacts, not a Cargo target cache."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tempfile

FILES = ("libphux_client_ffi.a", "phux")
CACHE = Path("target/ci-cockpit-artifacts")
OUTPUT = Path("target/ffi-release")
BUILD_ENV = {"RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
             "CARGO_ENCODED_RUSTFLAGS", "CARGO_ENCODED_RUSTDOCFLAGS", "CARGO_BUILD_TARGET",
             "CARGO_BUILD_RUSTFLAGS", "MACOSX_DEPLOYMENT_TARGET", "SDKROOT", "DEVELOPER_DIR",
             "CC", "CXX", "AR", "CFLAGS", "CXXFLAGS", "LDFLAGS", "ZIG",
             "LIBGHOSTTY_VT_SYS_OPTIMIZE"}


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def build_identity():
    if command("git", "status", "--porcelain", "--untracked-files=normal"):
        raise ValueError("exact-input artifacts require a clean checkout")
    if any(key in os.environ for key in ("GHOSTTY_SOURCE_DIR", "GHOSTTY_ZIG_SYSTEM_DIR")):
        raise ValueError("external Ghostty source/system overrides cannot use exact-tree artifacts")
    return {
        "schema": 1,
        "tree": command("git", "rev-parse", "HEAD^{tree}"),
        "rust": command("rustc", "-vV"),
        "zig": command("zig", "version"),
        # Official and nixpkgs Zig can report the same version while using
        # different LLVM builds (observed in the browser-engine audit).
        "zig_sha256": digest(Path(shutil.which("zig")).resolve()),
        "platform": platform.platform(),
        "cpu": command("sysctl", "-n", "machdep.cpu.brand_string"),
        "xcode": command("xcodebuild", "-version"),
        "sdk": command("xcrun", "--show-sdk-version"),
        "profile": "ffi-release",
        "environment": {key: value for key, value in os.environ.items()
                        if key in BUILD_ENV or key.startswith(("CARGO_PROFILE_", "CARGO_TARGET_"))},
    }


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def cache_key(identity):
    encoded = json.dumps(identity, sort_keys=True).encode()
    return "cockpit-rust-artifacts-v1-" + hashlib.sha256(encoded).hexdigest()


def save(identity, source=OUTPUT, destination=CACHE):
    destination.mkdir(parents=True, exist_ok=True)
    hashes = {}
    for name in FILES:
        shutil.copy2(source / name, destination / name)
        hashes[name] = digest(destination / name)
    (destination / "manifest.json").write_text(
        json.dumps({"identity": identity, "files": hashes}, sort_keys=True) + "\n"
    )


def read_manifest(identity, source):
    manifest = json.loads((source / "manifest.json").read_text())
    if manifest["identity"] != identity or set(manifest["files"]) != set(FILES):
        raise ValueError("artifact manifest identity or output set does not match")
    return manifest["files"]


def outputs_match(hashes, directory):
    return all(digest(directory / name) == hashes[name] for name in FILES)


def atomic_copy(source, destination):
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
        staged = Path(temporary) / destination.name
        shutil.copy2(source, staged)
        os.replace(staged, destination)


def restore(identity, source=CACHE, destination=OUTPUT):
    hashes = read_manifest(identity, source)
    if not outputs_match(hashes, source):
        return False
    destination.mkdir(parents=True, exist_ok=True)
    for name in FILES:
        atomic_copy(source / name, destination / name)
    return True


def verify(identity, source=CACHE, destination=OUTPUT):
    # Zig can link while its coordinator staging step runs. Verification must
    # never truncate or replace an output under those concurrent readers.
    return outputs_match(read_manifest(identity, source), destination)


def attempt(operation, identity):
    try:
        return operation(identity)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"No valid exact-input Cockpit artifacts; building: {error}")
        return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["key", "restore", "save", "verify"])
    mode = parser.parse_args().mode
    identity = build_identity()
    if mode == "verify":
        sys.exit(0 if attempt(verify, identity) else 1)
    result = {"key": cache_key(identity)}
    if mode == "restore":
        result["restored"] = str(attempt(restore, identity)).lower()
    elif mode == "save":
        save(identity)
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf8") as stream:
        for key, value in result.items():
            stream.write(f"{key}={value}\n")


if __name__ == "__main__":
    main()
