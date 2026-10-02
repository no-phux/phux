#!/usr/bin/env python3
"""Check or refresh the native demo's immutable phux source pin.

Checking reads the pinned Git object when available; a shallow or squash-merged
checkout falls back to the checksum-verified GitHub archive. Refreshing downloads
that archive only after its source commit is pushed.
The lockfile transform mirrors Cargo's local libghostty patch: only the two
libghostty Git source entries disappear; every dependency version stays pinned.
"""

import argparse
import hashlib
import io
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
DOCKERFILE = Path("docs/site/worker/Dockerfile")
LIBGHOSTTY_SOURCE = "git+https://github.com/phall1/libghostty-rs.git"


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def git_file(root: Path, revision: str, path: str) -> bytes:
    try:
        return subprocess.check_output(
            ["git", "show", f"{revision}:{path}"], cwd=root, stderr=subprocess.PIPE,
        )
    except subprocess.CalledProcessError as error:
        raise ValueError(
            f"cannot read {path} at {revision}; fetch that commit or use a full-history checkout"
        ) from error


def download_archive(revision: str) -> bytes:
    return subprocess.check_output([
        "curl", "--fail", "--location", "--silent", "--show-error", "--retry", "3",
        "--max-time", "120", f"https://github.com/no-phux/phux/archive/{revision}.tar.gz",
    ])


def archive_inputs(revision: str, archive: bytes) -> tuple[bytes, bytes]:
    # Read only these two files, never extract untrusted tar paths or symlinks.
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as source:
        files = []
        for path in ("Cargo.toml", "Cargo.lock"):
            member = source.getmember(f"phux-{revision}/{path}")
            if not member.isfile():
                raise ValueError(f"GitHub archive {path} is not a regular file")
            files.append(source.extractfile(member).read())
    return tuple(files)


def pinned_inputs(root: Path, revision: str, checksum: str) -> tuple[bytes, bytes]:
    try:
        return git_file(root, revision, "Cargo.toml"), git_file(root, revision, "Cargo.lock")
    except ValueError:
        # Squash/rebase merges do not preserve the release PR's source commit
        # in main's ancestry. A full-history clone alone cannot recover it.
        archive = download_archive(revision)
        if digest(archive) != checksum:
            raise ValueError(f"GitHub archive checksum does not match PHUX_SOURCE_SHA256 for {revision}")
        return archive_inputs(revision, archive)


def source_metadata(manifest: bytes, lock: bytes) -> dict[str, str]:
    cargo = tomllib.loads(manifest.decode())
    version = cargo["workspace"]["package"]["version"]
    revision = cargo["workspace"]["dependencies"]["libghostty-vt"]["rev"]
    data = tomllib.loads(lock.decode())
    packages = {p["name"]: p for p in data["package"] if "source" not in p}
    if not packages or any(p["version"] != version for p in packages.values()):
        raise ValueError("pinned source Cargo.lock path versions disagree with its workspace version")
    expected_source = f"{LIBGHOSTTY_SOURCE}?rev={revision}#{revision}"
    patched = lock
    for name in ("libghostty-vt", "libghostty-vt-sys"):
        matches = [p for p in data["package"] if p["name"] == name]
        if len(matches) != 1 or matches[0].get("source") != expected_source:
            raise ValueError(f"pinned source {name} does not resolve to libghostty revision {revision}")
    source_line = f'source = "{expected_source}"\n'.encode()
    if patched.count(source_line) != 2:
        raise ValueError("expected exactly two libghostty Git source lines")
    patched = patched.replace(source_line, b"")
    return {
        "PHUX_VERSION": version,
        "PHUX_LOCK_SHA256": digest(lock),
        "PHUX_PATCHED_LOCK_SHA256": digest(patched),
        "LIBGHOSTTY_REVISION": revision,
    }


def arguments(dockerfile: str) -> dict[str, str]:
    args = {}
    for name, value in re.findall(r"^ARG (PHUX_[A-Z0-9_]+|LIBGHOSTTY_REVISION)=([^\n]+)$", dockerfile, re.M):
        if name in args and args[name] != value:
            raise ValueError(f"Dockerfile has conflicting {name} values")
        args[name] = value
    for name in ("PHUX_REVISION", "LIBGHOSTTY_REVISION"):
        if re.fullmatch(r"[0-9a-f]{40}", args.get(name, "")) is None:
            raise ValueError(f"Dockerfile must pin a full {name}")
    for name in ("PHUX_SOURCE_SHA256", "PHUX_LOCK_SHA256", "PHUX_PATCHED_LOCK_SHA256"):
        if re.fullmatch(r"[0-9a-f]{64}", args.get(name, "")) is None:
            raise ValueError(f"Dockerfile must pin {name}")
    return args


def check(root: Path, dockerfile: str) -> None:
    args = arguments(dockerfile)
    revision = args["PHUX_REVISION"]
    metadata = source_metadata(*pinned_inputs(root, revision, args["PHUX_SOURCE_SHA256"]))
    source_metadata((root / "Cargo.toml").read_bytes(), (root / "Cargo.lock").read_bytes())
    workspace = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
    if metadata["PHUX_VERSION"] != workspace["package"]["version"]:
        raise ValueError(
            f"native demo source {revision} is phux {metadata['PHUX_VERSION']}, "
            f"but the workspace is {workspace['package']['version']}; refresh the entire source pin"
        )
    if metadata["LIBGHOSTTY_REVISION"] != workspace["dependencies"]["libghostty-vt"]["rev"]:
        raise ValueError("native demo source libghostty revision differs from the workspace")
    for name, expected in metadata.items():
        if args[name] != expected:
            raise ValueError(f"Dockerfile {name} is {args[name]}, expected {expected} from source {revision}")
    source_url = f"https://github.com/no-phux/phux/archive/{revision}.tar.gz"
    archive = f'ADD --checksum=sha256:{args["PHUX_SOURCE_SHA256"]} \\\n    {source_url} /tmp/phux.tar.gz'
    if archive not in dockerfile:
        raise ValueError("Dockerfile phux archive URL/checksum disagrees with its ARG pins")
    for name in ("PHUX_VERSION", "PHUX_REVISION", "PHUX_SOURCE_SHA256"):
        if f'test "${name}" = "{args[name]}"' not in dockerfile:
            raise ValueError(f"Dockerfile must assert its {name} pin")


def refresh(root: Path, dockerfile: str, revision: str, archive: bytes) -> str:
    old = arguments(dockerfile)
    manifest = git_file(root, revision, "Cargo.toml")
    lock = git_file(root, revision, "Cargo.lock")
    archived = archive_inputs(revision, archive)
    for path, actual, expected in zip(("Cargo.toml", "Cargo.lock"), archived, (manifest, lock)):
        if actual != expected:
            raise ValueError(f"GitHub archive {path} differs from source commit {revision}")
    values = source_metadata(manifest, lock)
    if values["LIBGHOSTTY_REVISION"] != old["LIBGHOSTTY_REVISION"]:
        raise ValueError("update the Dockerfile's external libghostty/Ghostty pins before refreshing phux")
    values.update(PHUX_REVISION=revision, PHUX_SOURCE_SHA256=digest(archive))
    result = dockerfile
    # Replace immutable hashes everywhere they occur (ARG, ADD, and RUN guard).
    # The version remains owned by release-please's annotated regions.
    if values["PHUX_VERSION"] != old["PHUX_VERSION"]:
        raise ValueError("release-please must synchronize PHUX_VERSION before refreshing its source pin")
    for name in ("PHUX_REVISION", "PHUX_SOURCE_SHA256", "PHUX_LOCK_SHA256", "PHUX_PATCHED_LOCK_SHA256"):
        result = result.replace(old[name], values[name])
    check(root, result)
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--revision", help="already-pushed source commit to pin")
    args = parser.parse_args()
    try:
        path = ROOT / DOCKERFILE
        before = path.read_text()
        if args.check:
            check(ROOT, before)
        else:
            revision = subprocess.check_output(
                ["git", "rev-parse", "--verify", f"{args.revision}^{{commit}}"], cwd=ROOT, text=True,
            ).strip()
            # Do not recursively re-pin the previous pin-only commit.
            try:
                check(ROOT, before)
                previous = arguments(before)["PHUX_REVISION"]
                unchanged = subprocess.run([
                    "git", "diff", "--quiet", previous, revision, "--", ".",
                    f":(exclude){DOCKERFILE.as_posix()}",
                ], cwd=ROOT, check=False).returncode == 0
            except ValueError:
                unchanged = False
            if unchanged:
                print("native demo source pin already covers this source snapshot")
                return 0
            archive = download_archive(revision)
            after = refresh(ROOT, before, revision, archive)
            if after != before:
                path.write_text(after)
        print("native demo source version and lock pins are consistent")
        return 0
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError, tarfile.TarError) as error:
        print(f"native demo source pin: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
