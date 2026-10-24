#!/usr/bin/env python3
"""Open consumer PRs for an immutable private PHUX brand release."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import brand

SOURCE = "no-phux/phux-brand"
REPOSITORIES = ("phux", "phux-mobile", "website")


def run(args, cwd=None, source=False):
    env = os.environ.copy()
    if source:
        env["GH_TOKEN"] = env.get("BRAND_READ_TOKEN", env.get("GH_TOKEN", ""))
    return subprocess.check_output(args, cwd=cwd, env=env, text=True).strip()


def api(endpoint):
    return json.loads(run(["gh", "api", endpoint], source=True))


def approved_release_tag(value):
    if value["draft"] or value["prerelease"] or not value.get("immutable"):
        raise ValueError("brand automation accepts only published immutable stable releases")
    tag = value["tag_name"]
    if not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
        raise ValueError("unexpected brand release tag")
    return tag


def archive_checksum(value):
    assets = [item for item in value["assets"] if item["name"] == "phux-brand.zip"]
    if len(assets) != 1 or not re.fullmatch(r"sha256:[0-9a-f]{64}", assets[0].get("digest", "")):
        raise ValueError("release lacks a uniquely checksummed brand archive")
    return assets[0]["digest"].split(":")[1]


def release(version):
    if version and not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise ValueError("version must be a stable semantic version")
    endpoint = f"tags/v{version}" if version else "latest"
    value = api(f"repos/{SOURCE}/releases/{endpoint}")
    return approved_release_tag(value), archive_checksum(value)


def git(root, *args):
    return run(["git", "-c", "credential.helper=", "-c", "credential.helper=!gh auth git-credential", *args], cwd=root)


def public_kit(root, archive, checksum, manifest):
    version = manifest["version"]
    target = root / "public/brand/releases" / version
    target.mkdir(parents=True, exist_ok=True)
    existing = target / "phux-brand.zip"
    if existing.exists() and brand.digest(existing.read_bytes()) != checksum:
        raise ValueError("refusing to replace an already-published brand version")
    shutil.copyfile(archive, existing)
    (target / "SHA256SUMS").write_text(f"{checksum}  phux-brand.zip\n")
    (target / "manifest.json").write_bytes(brand.encode(manifest))
    index_path = root / "public/brand/releases/index.json"
    index = json.loads(index_path.read_bytes()) if index_path.exists() else {"schema": 1, "releases": {}}
    index["releases"][version] = {"sha256": checksum, "source_revision": manifest["source_revision"]}
    index_path.write_bytes(brand.encode(index))


def newer(proposed, current):
    return tuple(map(int, proposed.split("."))) > tuple(map(int, current.split(".")))


def propose(directory, repository, archive, checksum, manifest, rollback=False):
    root = directory / repository
    git(directory, "clone", "--quiet", "--depth", "1", "--branch", "main", f"https://github.com/no-phux/{repository}.git", str(root))
    old = brand.check(root)
    version = manifest["version"]
    if old["version"] == version:
        if old["sha256"] != checksum:
            raise ValueError(f"{repository}: published version changed checksum")
        return f"{repository}: already pinned to {version}"
    if not rollback and not newer(version, old["version"]):
        return f"{repository}: newer release already installed; skipping {version}"
    branch = f"chore/brand-v{version}"
    repo = f"no-phux/{repository}"
    existing = json.loads(run(["gh", "pr", "list", "--repo", repo, "--head", branch, "--base", "main", "--json", "url"]))
    if existing:
        return existing[0]["url"]
    git(root, "switch", "-c", branch)
    brand.install(root, archive, checksum)
    brand.check(root)
    if repository == "website":
        public_kit(root, archive, checksum, manifest)
    targets = ["brand.lock.json", *json.loads((root / "brand.config.json").read_bytes())["targets"].values()]
    if repository == "website":
        targets.append("public/brand/releases")
    git(root, "add", "--", *targets)
    git(root, "-c", "user.name=PHUX Brand Bot", "-c", "user.email=phux-brand[bot]@users.noreply.github.com",
        "-c", "core.hooksPath=/dev/null", "commit", "-m", f"chore(brand): update approved assets to {version}")
    git(root, "-c", "core.hooksPath=/dev/null", "push", "origin", branch)
    body = directory / f"{repository}-body.md"
    body.write_text(f"Update approved PHUX brand assets from {old['version']} to {version}.\n\n"
                    f"Source revision: `{manifest['source_revision']}`.\n"
                    f"Archive SHA-256: `{checksum}`.\n\n"
                    "Verified the complete release manifest and this consumer's committed assets. "
                    "Normal builds require no private repository access. Consumer CI remains the merge gate.\n")
    return run(["gh", "pr", "create", "--repo", repo, "--base", "main", "--head", branch,
                "--title", f"chore(brand): update assets to {version}", "--body-file", str(body)])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version")
    parser.add_argument("--allow-rollback", action="store_true")
    args = parser.parse_args()
    if args.allow_rollback and not args.version:
        parser.error("rollback requires an explicit --version")
    tag, checksum = release(args.version)
    with tempfile.TemporaryDirectory(prefix="phux-brand-update-") as directory:
        directory = Path(directory)
        run(["gh", "release", "download", tag, "--repo", SOURCE, "--pattern", "phux-brand.zip", "--dir", str(directory)], source=True)
        archive = directory / "phux-brand.zip"
        manifest, _ = brand.read_bundle(archive, checksum)
        revision = api(f"repos/{SOURCE}/commits/{tag}")["sha"]
        if manifest["version"] != tag[1:] or manifest["source_revision"] != revision:
            raise ValueError("brand manifest does not match the published release tag")
        failures = []
        for repository in REPOSITORIES:
            try:
                print(propose(directory, repository, archive, checksum, manifest, args.allow_rollback))
            except (ValueError, OSError, subprocess.CalledProcessError) as error:
                failures.append(f"{repository}: {error}")
        if failures:
            raise RuntimeError("\n".join(failures))


if __name__ == "__main__":
    main()
