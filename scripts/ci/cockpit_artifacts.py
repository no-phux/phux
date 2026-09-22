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

KIND_FILES = {
    "ffi": ("libphux_client_ffi.a",),
    "cli": ("phux",),
}
FILES = KIND_FILES["ffi"] + KIND_FILES["cli"]
CACHE = Path("target/ci-cockpit-artifacts")
OUTPUT = Path("target/ffi-release")
SHARED_INPUTS = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml")
# Only these paths enter a kind's digest. Cockpit Node tests and other lanes
# may leave dirt elsewhere; that must not break Rust artifact identity.
INPUT_PATHS = (*SHARED_INPUTS, ".cargo", "crates")
BUILD_ENV = {"RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
             "CARGO_ENCODED_RUSTFLAGS", "CARGO_ENCODED_RUSTDOCFLAGS", "CARGO_BUILD_TARGET",
             "CARGO_BUILD_RUSTFLAGS", "MACOSX_DEPLOYMENT_TARGET", "SDKROOT", "DEVELOPER_DIR",
             "CC", "CXX", "AR", "CFLAGS", "CXXFLAGS", "LDFLAGS", "ZIG",
             "LIBGHOSTTY_VT_SYS_OPTIMIZE"}


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def non_library(path):
    parts = path.split("/")
    return len(parts) >= 4 and parts[0] == "crates" and parts[2] in {"tests", "benches", "examples"}


def select_input_lines(lines, directories):
    """Index lines whose blobs enter one binary. Tests, benches and examples do not."""
    selected = []
    for line in lines:
        if "\t" not in line:
            continue
        path = line.split("\t", 1)[1]
        if non_library(path):
            continue
        if path in SHARED_INPUTS or path.startswith(".cargo/"):
            selected.append(line)
            continue
        parts = path.split("/")
        if len(parts) >= 2 and parts[0] == "crates" and parts[1] in directories:
            selected.append(line)
    return selected


def input_digest(kind):
    classifier = load_classifier()
    root = "phux-client-ffi" if kind == "ffi" else "phux"
    directories = classifier.closure_directories(root)
    listed = command("git", "ls-files", "-s", "--", *INPUT_PATHS)
    lines = select_input_lines(listed.splitlines(), directories)
    return hashlib.sha256("\n".join(lines).encode()).hexdigest()


def load_classifier():
    import importlib.util
    path = Path(__file__).with_name("classify-changes.py")
    spec = importlib.util.spec_from_file_location("phux_classify_changes", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def require_clean_inputs():
    """Refuse identity when an input path is dirty; ignore unrelated worktree dirt."""
    if command("git", "status", "--porcelain", "--untracked-files=normal", "--", *INPUT_PATHS):
        raise ValueError("exact-input artifacts require a clean checkout")


def build_identity(kind="cli"):
    if kind not in KIND_FILES:
        raise ValueError(f"unknown artifact kind {kind}")
    require_clean_inputs()
    if any(key in os.environ for key in ("GHOSTTY_SOURCE_DIR", "GHOSTTY_ZIG_SYSTEM_DIR")):
        raise ValueError("external Ghostty source/system overrides cannot use exact-tree artifacts")
    return {
        "schema": 2,
        "kind": kind,
        "inputs": input_digest(kind),
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
    return "cockpit-rust-artifacts-v2-" + hashlib.sha256(encoded).hexdigest()


def save(identity, source=OUTPUT, destination=CACHE, names=FILES):
    destination.mkdir(parents=True, exist_ok=True)
    hashes = {}
    for name in names:
        shutil.copy2(source / name, destination / name)
        hashes[name] = digest(destination / name)
    (destination / "manifest.json").write_text(
        json.dumps({"identity": identity, "files": hashes}, sort_keys=True) + "\n"
    )


def read_manifest(identity, source, names=FILES):
    """Accept the present set that save wrote — full FILES or one KIND_FILES entry."""
    manifest = json.loads((source / "manifest.json").read_text())
    if manifest["identity"] != identity or set(manifest["files"]) != set(names):
        raise ValueError("artifact manifest identity or output set does not match")
    return manifest["files"]


def outputs_match(hashes, directory, names=None):
    names = tuple(hashes) if names is None else names
    return all(digest(directory / name) == hashes[name] for name in names)


def atomic_copy(source, destination):
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
        staged = Path(temporary) / destination.name
        shutil.copy2(source, staged)
        os.replace(staged, destination)


def restore(identity, source=CACHE, destination=OUTPUT, names=None):
    expected = FILES if names is None else names
    hashes = read_manifest(identity, source, expected)
    names = tuple(hashes) if names is None else names
    if not outputs_match(hashes, source, names):
        return False
    destination.mkdir(parents=True, exist_ok=True)
    for name in names:
        atomic_copy(source / name, destination / name)
    return True


def verify(identity, source=CACHE, destination=OUTPUT, names=None):
    # Zig can link while its coordinator staging step runs. Verification must
    # never truncate or replace an output under those concurrent readers.
    expected = FILES if names is None else names
    hashes = read_manifest(identity, source, expected)
    return outputs_match(hashes, destination, tuple(hashes) if names is None else names)


def attempt(operation, identity):
    try:
        return operation(identity)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"No valid exact-input Cockpit artifacts; building: {error}")
        return False


def kind_cache(kind):
    return CACHE / kind


def save_present(kind):
    names = KIND_FILES[kind]
    if not all((OUTPUT / name).is_file() for name in names):
        print(f"skip save {kind}: output missing")
        return False
    save(build_identity(kind), OUTPUT, kind_cache(kind), names)
    return True


def restore_kind(kind):
    identity = build_identity(kind)
    return restore(identity, kind_cache(kind), OUTPUT, KIND_FILES[kind])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["key", "restore", "save", "verify"])
    mode = parser.parse_args().mode
    if mode == "verify":
        # The CLI stager calls this. It must not rebuild, and it must not
        # rewrite the archive a concurrent Zig link is reading.
        sys.exit(0 if attempt(lambda _identity: verify(
            build_identity("cli"), kind_cache("cli"), OUTPUT, KIND_FILES["cli"]
        ), None) else 1)
    result = {}
    if mode == "key":
        result = {f"{kind}_key": cache_key(build_identity(kind)) for kind in KIND_FILES}
    elif mode == "restore":
        result = {f"{kind}_restored": str(attempt(lambda _identity, kind=kind: restore_kind(kind), None)).lower()
                  for kind in KIND_FILES}
    elif mode == "save":
        for kind in KIND_FILES:
            save_present(kind)
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf8") as stream:
        for key, value in result.items():
            stream.write(f"{key}={value}\n")


if __name__ == "__main__":
    main()
