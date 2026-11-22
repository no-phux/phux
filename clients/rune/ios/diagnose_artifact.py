#!/usr/bin/env python3
"""Acquire the single immutable failed-run app for diagnostic use only."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import stat
import subprocess
import urllib.error
import urllib.parse
import urllib.request
import unicodedata
import zipfile

REPOSITORY = "no-phux/phux"
ARTIFACT = 14755772123
RUN = 45018627963
HEAD = "aa3d84764431626f24e5deae7dbaa8d4c269ed09"
BUILD = "5006577de5e2d6759f32edc0d8fa011559a78397"
SIZE = 85636037
SHA256 = "afeb217a8dffced9c920dd037c3a1476fa2f44b377ad234d00160ac925ac1e1b"
API = f"https://api.github.com/repos/{REPOSITORY}/actions/artifacts/{ARTIFACT}"
APP = "ios-simulator/Phux Rune.app"
ALLOWED_CHANGES = frozenset((
    "clients/rune/Cargo.toml", "clients/rune/Cargo.lock", "clients/rune/RELEASE_NOTES.md",
    ".github/workflows/rune-native-diagnose.yml",
    ".github/workflows/rune-macos.yml",
    "clients/rune/ios/diagnose_artifact.py", "clients/rune/ios/diagnose_commands.py",
    "clients/rune/ios/connection-diagnostics.md",
    "clients/rune/scripts/test_ios_command_diagnostics.py",
    "clients/rune/scripts/test_ios_command_capture.py",
))


def verify_sources(root):
    changed = subprocess.run(
        ["git", "diff", "--name-only", HEAD, "HEAD"], cwd=root,
        capture_output=True, text=True, check=True, timeout=30).stdout.splitlines()
    if set(changed) - ALLOWED_CHANGES:
        raise ValueError("source differs from original outside diagnostic/release metadata paths")


def validate_metadata(metadata):
    expected = {"id": ARTIFACT, "name": "qualification-ios-native", "size_in_bytes": SIZE,
                "digest": "sha256:" + SHA256, "expired": False}
    if any(metadata.get(key) != value for key, value in expected.items()):
        raise ValueError("original artifact metadata mismatch")
    run = metadata.get("workflow_run", {})
    expected_run = {"id": RUN, "head_sha": HEAD, "repository_id": 1249328322,
                    "head_repository_id": 1249328322}
    if any(run.get(key) != value for key, value in expected_run.items()):
        raise ValueError("original artifact run provenance mismatch")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, newurl):
        return None


def api_request(url, token):
    return urllib.request.Request(url, headers={
        "Authorization": "Bearer " + token, "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "rune-native-diagnostic",
    })


def download_url(token):
    # Never forward the API credential to the signed blob URL on another host.
    opener = urllib.request.build_opener(NoRedirect())
    try:
        with opener.open(api_request(API + "/zip", token), timeout=60):
            raise ValueError("artifact API did not return its download redirect")
    except urllib.error.HTTPError as error:
        if error.code != 302:
            raise ValueError("artifact download API failed") from None
        location = error.headers.get("Location", "")
        error.close()
    parsed = urllib.parse.urlsplit(location)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
        raise ValueError("unsafe artifact download redirect")
    return location


def save_archive(url, destination):
    digest, total = hashlib.sha256(), 0
    with urllib.request.urlopen(url, timeout=60) as response, destination.open("xb") as target:
        while chunk := response.read(min(1024 * 1024, SIZE + 1 - total)):
            total += len(chunk)
            if total > SIZE:
                raise ValueError("artifact download exceeds immutable size")
            digest.update(chunk)
            target.write(chunk)
    if total != SIZE or digest.hexdigest() != SHA256:
        raise ValueError("original artifact byte length or SHA-256 mismatch")


def archive_path(info):
    path = PurePosixPath(info.filename)
    if (not info.filename or info.filename != info.orig_filename
            or "\\" in info.filename or ":" in info.filename or path.is_absolute()
            or any(part in ("", ".", "..") for part in info.filename.rstrip("/").split("/"))):
        raise ValueError("unsafe artifact member path")
    return path


def archive_member(info):
    path = archive_path(info)
    mode = info.external_attr >> 16
    kind = stat.S_IFMT(mode)
    if kind not in (stat.S_IFREG, stat.S_IFDIR):
        raise ValueError("artifact contains a symlink or special file")
    if info.is_dir() != (kind == stat.S_IFDIR):
        raise ValueError("artifact member type mismatch")
    if stat.S_IMODE(mode) not in (0o600, 0o644, 0o755):
        raise ValueError("unsafe artifact member permissions")
    if info.flag_bits & 1 or info.file_size > 256 * 1024**2:
        raise ValueError("encrypted or oversized artifact member")
    return path


def checked_members(archive):
    members, seen, total = [], set(), 0
    if len(archive.infolist()) > 1024:
        raise ValueError("too many artifact members")
    for info in archive.infolist():
        path = archive_member(info)
        name = unicodedata.normalize("NFD", str(path)).casefold()
        if name in seen:
            raise ValueError("duplicate artifact member")
        seen.add(name)
        total += info.file_size
        members.append((info, path))
    if total > 512 * 1024**2:
        raise ValueError("artifact expansion exceeds limit")
    files = {unicodedata.normalize("NFD", str(path)).casefold()
             for info, path in members if not info.is_dir()}
    if any(unicodedata.normalize("NFD", str(parent)).casefold() in files
           for _, path in members for parent in path.parents):
        raise ValueError("artifact file/directory collision")
    return members


def extract_app(source, destination):
    # Destination is new, private, and never an existing app or user's device.
    destination.mkdir(mode=0o700)
    with zipfile.ZipFile(source) as archive:
        members = checked_members(archive)
        if archive.testzip() is not None:
            raise ValueError("artifact CRC mismatch")
        for info, path in members:
            if not (str(path).startswith(APP + "/") or str(path) == "ios-simulator/build.json"):
                continue
            target = destination.joinpath(*path.parts)
            if info.is_dir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            with archive.open(info) as original, target.open("xb") as output:
                shutil.copyfileobj(original, output, length=1024 * 1024)
            target.chmod(stat.S_IMODE(info.external_attr >> 16))
    build = json.loads((destination / "ios-simulator/build.json").read_text())
    if build.get("revision") != BUILD or build.get("target") != "aarch64-apple-ios-sim":
        raise ValueError("original simulator build provenance mismatch")
    return destination / APP


def prepare(root, out):
    verify_sources(root)
    out.mkdir(mode=0o700, parents=True, exist_ok=False)
    token = os.environ.get("GITHUB_TOKEN")
    if not token:
        raise ValueError("GITHUB_TOKEN is required only for artifact acquisition")
    with urllib.request.urlopen(api_request(API, token), timeout=60) as response:
        metadata = json.load(response)
    validate_metadata(metadata)
    archive = out / "original.zip"
    save_archive(download_url(token), archive)
    app = extract_app(archive, out / "original")
    # No signed URLs, token, runner identity, endpoints, or config in provenance.
    (out / "provenance.json").write_text(json.dumps({
        "diagnostic_only": True, "repository": REPOSITORY, "artifact_id": ARTIFACT,
        "run_id": RUN, "head_sha": HEAD, "build_revision": BUILD,
        "bytes": SIZE, "sha256": SHA256, "source_equivalence_checked": True,
    }, indent=2) + "\n")
    return app


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[3]
    try:
        prepare(root, args.out.resolve())
    except Exception:
        # urllib exceptions can include a signed URL. Never print their details.
        parser.exit(1, "Immutable diagnostic artifact acquisition failed; no qualification ran.\n")


if __name__ == "__main__":
    main()
