#!/usr/bin/env python3
"""Install and verify PHUX brand releases. Standard library; no build-time network."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys
import tempfile
import urllib.request
import zipfile

MAX_BYTES = 100 * 1024 * 1024
PUBLIC_BASE = "https://phux.sh/brand/releases"


def digest(data):
    return hashlib.sha256(data).hexdigest()


def encode(value):
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def safe_name(name):
    if not isinstance(name, str) or not name:
        raise ValueError("invalid asset path")
    path = PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts or name == ".":
        raise ValueError(f"unsafe asset path: {name}")
    if str(path) != name or re.search(r"[\x00-\x1f\\]", name):
        raise ValueError(f"noncanonical asset path: {name}")
    return path


def destination(root, name):
    path = root.joinpath(*safe_name(name).parts)
    if path.is_symlink() or any(p.is_symlink() for p in path.parents if p != root.parent):
        raise ValueError(f"symlink in asset destination: {name}")
    if not path.resolve().is_relative_to(root):
        raise ValueError(f"asset destination escapes repository: {name}")
    return path


def config(root):
    value = json.loads((root / "brand.config.json").read_bytes())
    if value.get("schema") != 1 or value.get("consumer") not in ("phux", "mobile", "website"):
        raise ValueError("unsupported consumer configuration")
    targets = value["targets"]
    if not targets or len(set(targets.values())) != len(targets):
        raise ValueError("empty or duplicate consumer destinations")
    for name, target in targets.items():
        safe_name(name)
        destination(root, target)
        if target in ("brand.lock.json", "brand.config.json") or target.startswith(".git/"):
            raise ValueError("reserved consumer destination")
    return value


def archive_names(archive):
    infos = archive.infolist()
    names = [item.filename for item in infos]
    if len(names) != len(set(names)) or len(names) > 4096:
        raise ValueError("duplicate entries or too many brand files")
    if sum(i.file_size for i in infos) > MAX_BYTES:
        raise ValueError("oversized brand bundle")
    for item in infos:
        safe_name(item.filename)
        if item.is_dir() or stat.S_ISLNK(item.external_attr >> 16):
            raise ValueError("brand bundle must contain regular files only")
    return names


def unpack(data):
    if len(data) > MAX_BYTES:
        raise ValueError("brand bundle exceeds size limit")
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        names = archive_names(archive)
        files = {name: archive.read(name) for name in names}
    manifest = json.loads(files.pop("manifest.json"))
    validate_manifest(manifest, files)
    return manifest, files


def validate_manifest(manifest, files):
    if manifest.get("schema") != 1 or not re.fullmatch(r"\d+\.\d+\.\d+", manifest.get("version", "")):
        raise ValueError("unsupported brand manifest or version")
    if not re.fullmatch(r"[0-9a-f]{40}", manifest.get("source_revision", "")):
        raise ValueError("brand manifest lacks source revision")
    if set(manifest["files"]) != set(files):
        raise ValueError("brand manifest does not match archive contents")
    for name, record in manifest["files"].items():
        if record["sha256"] != digest(files[name]) or record["bytes"] != len(files[name]):
            raise ValueError(f"brand asset checksum mismatch: {name}")


def read_bundle(path, expected):
    if not re.fullmatch(r"[0-9a-f]{64}", expected):
        raise ValueError("a SHA-256 checksum is required")
    data = Path(path).read_bytes()
    if digest(data) != expected:
        raise ValueError("brand bundle checksum mismatch")
    return unpack(data)


def atomic_write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".brand-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
        os.chmod(temporary, 0o644)
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def apply_changes(changes):
    for path, data in changes.items():
        if data is None:
            path.unlink(missing_ok=True)
        else:
            atomic_write(path, data)


def commit_files(changes):
    backups = {path: path.read_bytes() if path.exists() else None for path in changes}
    try:
        apply_changes(changes)
    except Exception:
        apply_changes(backups)
        raise


def obsolete_files(root, previous, targets):
    removed = {}
    for record in previous.get("files", {}).values():
        name = record["destination"]
        if name in targets.values():
            continue
        path = destination(root, name)
        if path.exists() and digest(path.read_bytes()) != record["sha256"]:
            raise ValueError(f"refusing to remove edited brand asset: {name}")
        removed[path] = None
    return removed


def install(root, archive, expected):
    root = root.resolve()
    settings = config(root)
    manifest, files = read_bundle(archive, expected)
    lock_path = destination(root, "brand.lock.json")
    previous = json.loads(lock_path.read_bytes()) if lock_path.exists() else {}
    targets = settings["targets"]
    if not set(targets).issubset(files):
        raise ValueError("brand release is missing required consumer assets")
    changes = obsolete_files(root, previous, targets)
    records = {}
    for name, target in targets.items():
        changes[destination(root, target)] = files[name]
        records[name] = {"destination": target, "sha256": digest(files[name])}
    lock = {
        "schema": 1, "consumer": settings["consumer"], "version": manifest["version"],
        "source_revision": manifest["source_revision"], "sha256": expected,
        "url": f"{PUBLIC_BASE}/{manifest['version']}/phux-brand.zip", "files": records,
    }
    # Commit the lock last. An interrupted installation fails verification and
    # is repaired by repeating the same checksum-pinned installation.
    changes[lock_path] = encode(lock)
    commit_files(changes)
    return lock


def check(root):
    root = root.resolve()
    settings = config(root)
    lock = json.loads((root / "brand.lock.json").read_bytes())
    if lock.get("schema") != 1 or lock.get("consumer") != settings["consumer"]:
        raise ValueError("brand lock does not match consumer")
    if set(lock["files"]) != set(settings["targets"]):
        raise ValueError("brand configuration changed; synchronize a release")
    for name, target in settings["targets"].items():
        record = lock["files"][name]
        if record["destination"] != target:
            raise ValueError(f"brand destination changed: {name}")
        if digest(destination(root, target).read_bytes()) != record["sha256"]:
            raise ValueError(f"brand asset drift: {target}; update in phux-brand and synchronize")
    return lock


def fetch(root):
    lock = json.loads((root / "brand.lock.json").read_bytes())
    expected_url = f"{PUBLIC_BASE}/{lock['version']}/phux-brand.zip"
    if lock["url"] != expected_url or not re.fullmatch(r"\d+\.\d+\.\d+", lock["version"]):
        raise ValueError("unsupported brand download URL")
    with urllib.request.urlopen(expected_url, timeout=60) as response:
        data = response.read(MAX_BYTES + 1)
    if len(data) > MAX_BYTES:
        raise ValueError("brand download exceeds size limit")
    with tempfile.TemporaryDirectory() as directory:
        archive = Path(directory) / "brand.zip"
        archive.write_bytes(data)
        return install(root, archive, lock["sha256"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["check", "sync", "restore"])
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--sha256")
    args = parser.parse_args()
    if args.command == "sync" and not (args.archive and args.sha256):
        parser.error("sync requires --archive and --sha256; restore uses the existing pin")
    operations = {
        "check": lambda: check(args.root),
        "restore": lambda: fetch(args.root),
        "sync": lambda: install(args.root, args.archive, args.sha256),
    }
    try:
        lock = operations[args.command]()
    except (ValueError, OSError, KeyError, TypeError, zipfile.BadZipFile) as error:
        print(f"brand: {error}", file=sys.stderr)
        return 1
    print(f"brand: {lock['consumer']} verified release {lock['version']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
