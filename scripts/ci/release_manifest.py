#!/usr/bin/env python3
"""Build and publish the channel manifests (ADR-0176).

`stable` derives channels/stable.json from the newest published releases.
`next` publishes one product's build onto the moving `next` prerelease and
records it in channels/next.json. `next-sha` prints the commit next.json names
for a product. `fixtures` renders the golden manifests every client tests.
"""

import argparse
import datetime
import hashlib
import json
import re
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

from publish_plan import parse_tag

REPO = "no-phux/phux"
DOWNLOAD = f"https://github.com/{REPO}/releases/download"
CHANNELS = ("stable", "next")
PRODUCTS = ("phux", "desktop")
TARGETS = {
    "phux": ("aarch64-apple-darwin", "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"),
    "desktop": ("aarch64-apple-darwin", "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"),
}
DESKTOP_ARCHIVES = {
    "aarch64-apple-darwin": "macos-arm64.zip",
    "x86_64-unknown-linux-gnu": "linux-x64.tar.gz",
    "aarch64-unknown-linux-gnu": "linux-arm64.tar.gz",
}
ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "scripts/fixtures/release-manifest"

# The POSIX installers parse the rendered file with awk, so values stay in
# this alphabet and the document has no arrays.
SAFE = re.compile(r"[A-Za-z0-9._:/+-]*")
KEY = re.compile(r"[A-Za-z0-9_-]+")
VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
SHA = re.compile(r"[0-9a-f]{40}")
DIGEST = re.compile(r"[0-9a-f]{64}")
PROTOCOL = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
PROTOCOL_SOURCE = re.compile(
    r"pub const PROTOCOL_VERSION: Version = Version \{\s*major: (\d+),\s*minor: (\d+),"
)


class ManifestError(Exception):
    pass


FAILURES = (ManifestError, subprocess.CalledProcessError, json.JSONDecodeError,
            KeyError, OSError)


def require(pattern, value, what):
    if not isinstance(value, str) or not pattern.fullmatch(value):
        raise ManifestError(f"{what} is not valid: {value!r}")
    return value


def release_of(product, channel, version):
    if channel == "next":
        return "next"
    return f"v{version}" if product == "phux" else f"desktop-v{version}"


def asset_name(product, channel, version, sha, target):
    """The naming rule every client derives on its own (ADR-0176 decision 1)."""
    if product == "phux":
        identity = f"next.{sha}" if channel == "next" else f"v{version}"
        return f"phux-{identity}-{target}.tar.gz"
    identity = f"next.{sha}" if channel == "next" else version
    return f"phux-desktop-{identity}-{DESKTOP_ARCHIVES[target]}"


def entry(product, channel, version, sha, protocol, digests):
    """One product's entry. `release`, `name`, `url` and `minimum_macos` are
    derived, and current clients ignore them; v0.87.0 and Desktop 0.26.0
    check them (ADR-0176 decision 8)."""
    targets = TARGETS[product]
    if set(digests) != set(targets):
        raise ManifestError(f"{product} needs digests for exactly {', '.join(targets)}")
    release = release_of(product, channel, version)
    artifacts = {}
    for target in targets:
        name = asset_name(product, channel, version, sha, target)
        artifacts[target] = {"name": name, "url": f"{DOWNLOAD}/{release}/{name}",
                             "sha256": require(DIGEST, digests[target], f"{name} sha256")}
    document = {
        "version": require(VERSION, version, f"{product} version"),
        "release": release,
        "sha": require(SHA, sha, f"{product} sha"),
        "protocol": require(PROTOCOL, protocol, f"{product} protocol"),
    }
    if product == "desktop":
        document["minimum_macos"] = "27.0"
    document["artifacts"] = artifacts
    return document


def manifest(channel, products, generated_at):
    return {
        "schema_version": 1,
        "channel": channel,
        "generated_at": generated_at,
        "products": {name: products[name] for name in PRODUCTS if name in products},
    }


def render(document):
    def check(value, path):
        if isinstance(value, dict):
            if not value:
                raise ManifestError(f"{path} is an empty object")
            for key, child in value.items():
                require(KEY, key, f"key at {path}")
                check(child, f"{path}.{key}")
        elif isinstance(value, str):
            require(SAFE, value, path)
        elif isinstance(value, bool) or not isinstance(value, int):
            raise ManifestError(f"{path} has unsupported type {type(value).__name__}")

    check(document, "manifest")
    return json.dumps(document, indent=2) + "\n"


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def describe(error):
    return f"{error} {getattr(error, 'stderr', None) or ''}".strip()


# --- git ---------------------------------------------------------------------


def run(*args):
    return subprocess.run(args, check=True, text=True, capture_output=True).stdout


def tag_commit(tag):
    """The commit `tag` names, fetching the tag when this checkout predates it."""
    try:
        return run("git", "rev-parse", f"{tag}^{{commit}}").strip()
    except subprocess.CalledProcessError:
        run("git", "fetch", "--quiet", "--no-tags", "origin", f"refs/tags/{tag}:refs/tags/{tag}")
        return run("git", "rev-parse", f"{tag}^{{commit}}").strip()


def protocol_at(ref, product):
    if product == "desktop":
        pin = run("git", "show", f"{ref}:clients/desktop/phux-protocol").strip()
        return require(PROTOCOL, pin, f"clients/desktop/phux-protocol at {ref}")
    match = PROTOCOL_SOURCE.search(run("git", "show", f"{ref}:crates/phux-protocol/src/lib.rs"))
    if not match:
        raise ManifestError(f"PROTOCOL_VERSION not found at {ref}")
    return f"{int(match.group(1))}.{int(match.group(2))}"


def version_at(ref, product):
    if product == "desktop":
        version = json.loads(run("git", "show", f"{ref}:clients/desktop/package.json"))["version"]
    else:
        cargo = tomllib.loads(run("git", "show", f"{ref}:Cargo.toml"))
        version = cargo["workspace"]["package"]["version"]
    return require(VERSION, version, f"{product} version at {ref}")


# --- GitHub ------------------------------------------------------------------


def gh_release(*args):
    return run("gh", "release", *args, "--repo", REPO)


def release(tag):
    """The release named `tag`, or None when there is none. Any other failure
    raises: an unreadable release is not an unpublished one."""
    try:
        return json.loads(run("gh", "api", f"repos/{REPO}/releases/tags/{tag}"))
    except subprocess.CalledProcessError as error:
        if "HTTP 404" in (error.stderr or ""):
            return None
        raise


def asset_digests(assets, names):
    """`{name: hex}` from the SHA-256 GitHub records for each uploaded asset."""
    recorded = {asset["name"]: asset.get("digest") or "" for asset in assets}
    digests = {}
    for name in names:
        if name not in recorded:
            raise ManifestError(f"{name} is not published")
        if not recorded[name].startswith("sha256:"):
            raise ManifestError(f"{name} has no recorded sha256")
        digests[name] = require(DIGEST, recorded[name].removeprefix("sha256:"), f"{name} sha256")
    return digests


def ensure_prerelease(tag, notes, target=None):
    """Create a prerelease that is never Latest, unless it already exists."""
    if release(tag) is not None:
        return
    args = ["create", tag, "--prerelease", "--latest=false", "--title", tag, "--notes", notes]
    try:
        gh_release(*args, *(["--target", target] if target else []))
    except subprocess.CalledProcessError:
        # publish.yml and next-release.yml can race to create `channels`.
        if release(tag) is None:
            raise


def upload(tag, paths):
    gh_release("upload", tag, *map(str, paths), "--clobber")


def published_manifest(channel):
    """The manifest clients see now, or None before the first publish."""
    channels = release("channels")
    if channels is None or f"{channel}.json" not in {a["name"] for a in channels["assets"]}:
        return None
    return json.loads(gh_release("download", "channels", "--pattern", f"{channel}.json",
                                 "--output", "-"))


def publish_manifest(document, current):
    name = f"{document['channel']}.json"
    if current is not None and {**current, "generated_at": None} == {**document, "generated_at": None}:
        print(f"{name} is already current")
        return
    ensure_prerelease("channels", "Release manifests: stable.json and next.json. Not a release.")
    with tempfile.TemporaryDirectory() as scratch:
        path = Path(scratch, name)
        path.write_text(render(document))
        upload("channels", [path])
    print(f"published {name}")


# --- stable ------------------------------------------------------------------


def newest_stable(rows):
    """The newest published `vX.Y.Z` and `desktop-vX.Y.Z` release rows."""
    newest = {}
    for row in rows:
        identity = parse_tag(row["tag_name"])
        if row["draft"] or row["prerelease"] or identity is None:
            continue
        product = identity["component"]
        if product in PRODUCTS and (product not in newest or identity["version"] > newest[product][0]):
            newest[product] = (identity["version"], row)
    return {product: row for product, (_, row) in newest.items()}


def stable_entry(product, row):
    tag = row["tag_name"]
    version = ".".join(map(str, parse_tag(tag)["version"]))
    sha = tag_commit(tag)
    names = {t: asset_name(product, "stable", version, None, t) for t in TARGETS[product]}
    digests = asset_digests(row["assets"], names.values())
    return entry(product, "stable", version, sha, protocol_at(sha, product),
                 {t: digests[name] for t, name in names.items()})


def stable(out, publish):
    listing = run("gh", "api", "--paginate", f"repos/{REPO}/releases?per_page=100", "--jq",
                  ".[] | {tag_name, draft, prerelease, assets: [.assets[] | {name, digest}]}")
    newest = newest_stable(json.loads(line) for line in listing.splitlines() if line)
    current = published_manifest("stable")
    products, failed = {}, False
    for product in PRODUCTS:
        if product not in newest:
            continue
        try:
            products[product] = stable_entry(product, newest[product])
        except FAILURES as error:
            # Keep what clients see now; the red job says the lane needs a look.
            print(f"error: stable {product}: {describe(error)}", file=sys.stderr)
            failed = True
            previous = (current or {}).get("products", {}).get(product)
            if previous:
                products[product] = previous
    if not products:
        raise ManifestError("nothing is published on stable")
    document = manifest("stable", products, now())
    Path(out).write_text(render(document))
    print(f"wrote {out}")
    if publish:
        publish_manifest(document, current)
    return 1 if failed else 0


# --- next --------------------------------------------------------------------


def verified_dist(product, sha, dist):
    """`{target: (path, hex)}` for each built asset whose sidecar matches it
    and carries a signature (ADR-0178)."""
    verified = {}
    for target in TARGETS[product]:
        name = asset_name(product, "next", None, sha, target)
        path = Path(dist, name)
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        if Path(dist, f"{name}.sha256").read_text().split() != [digest, name]:
            raise ManifestError(f"{name}.sha256 does not match {name}")
        if not Path(dist, f"{name}.sha256.minisig").is_file():
            raise ManifestError(f"{name}.sha256 is not signed")
        verified[target] = (path, digest)
    return verified


def bridge_record(product, sha, version, digests):
    """The pre-manifest next pointer an older binary reads (ADR-0176 decision 8)."""
    record = {"schema_version": 1, "channel": "next", "product": product,
              "sha": sha, "version": version}
    if product == "desktop":
        record["archive"] = asset_name("desktop", "next", None, sha, "aarch64-apple-darwin")
        record["sha256"] = digests["aarch64-apple-darwin"]
        record["minimum_macos"] = "27.0"
    record["published_at"] = now()
    name = "channel.json" if product == "phux" else "desktop-channel.json"
    return name, json.dumps(record, indent=2) + "\n"


def prune_next(product, keep):
    """Drop `product`'s next assets except the commits in `keep`, so a client
    still downloading the previous build does not 404."""
    family = "phux-next." if product == "phux" else "phux-desktop-next."
    kept = tuple(f"{family}{sha}-" for sha in keep)
    for asset in release("next")["assets"]:
        name = asset["name"]
        if name.startswith(family) and not name.startswith(kept):
            gh_release("delete-asset", "next", name, "--yes")
            print(f"pruned {name}")


def next_sha(product):
    current = published_manifest("next") or {}
    return current.get("products", {}).get(product, {}).get("sha", "")


def publish_next(product, sha, dist):
    """Publish `product` built at `sha` to the next channel.

    next-release.yml runs one at a time, so replacing this product's entry in
    the published next.json cannot lose the other product's update.
    """
    require(SHA, sha, "sha")
    built = verified_dist(product, sha, dist)
    version, protocol = version_at(sha, product), protocol_at(sha, product)
    current = published_manifest("next")

    ensure_prerelease("next", "Moving next channel (green main). Not a stable release.", sha)
    upload("next", [p for path, _ in built.values()
                    for p in (path, f"{path}.sha256", f"{path}.sha256.minisig")])
    recorded = asset_digests(release("next")["assets"], [path.name for path, _ in built.values()])
    if any(recorded[path.name] != digest for path, digest in built.values()):
        raise ManifestError("the uploaded bytes do not match the build")
    digests = {target: digest for target, (_, digest) in built.items()}

    products = dict((current or {}).get("products", {}))
    previous_sha = products.get(product, {}).get("sha")
    products[product] = entry(product, "next", version, sha, protocol, digests)
    publish_manifest(manifest("next", products, now()), current)

    with tempfile.TemporaryDirectory() as scratch:
        name, text = bridge_record(product, sha, version, digests)
        Path(scratch, name).write_text(text)
        upload("next", [Path(scratch, name)])

    if previous_sha:
        prune_next(product, {sha, previous_sha})
    print(f"published {product} {version} at {sha} on next")


# --- fixtures ----------------------------------------------------------------


def fixture(channel):
    digests = {t: f"{i:x}" * 64 for i, t in enumerate(TARGETS["phux"], 1)}
    return manifest(channel, {
        "phux": entry("phux", channel, "9.8.7", "a" * 40, "0.9", digests),
        "desktop": entry("desktop", channel, "1.2.3", "b" * 40, "0.9",
                         {t: f"{i:x}" * 64 for i, t in enumerate(TARGETS["desktop"], 13)}),
    }, "2026-01-01T00:00:00Z")


def fixtures(check):
    stale = []
    for channel in CHANNELS:
        path = FIXTURES / f"{channel}.json"
        text = render(fixture(channel))
        if not check:
            path.write_text(text)
        elif not path.exists() or path.read_text() != text:
            stale.append(str(path.relative_to(ROOT)))
    if stale:
        raise ManifestError(f"stale: {', '.join(stale)}; run {Path(__file__).name} fixtures")
    return 0


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    command = commands.add_parser("stable", help="build stable.json from the published releases")
    command.add_argument("--out", required=True)
    command.add_argument("--publish", action="store_true", help="upload it when it changed")
    command = commands.add_parser("next", help="publish one product's build to the next channel")
    command.add_argument("--product", choices=PRODUCTS, required=True)
    command.add_argument("--sha", required=True)
    command.add_argument("--dist", required=True)
    command = commands.add_parser("next-sha", help="print the commit next.json names for a product")
    command.add_argument("product", choices=PRODUCTS)
    command = commands.add_parser("fixtures", help="rewrite or check the golden fixtures")
    command.add_argument("--check", action="store_true")
    args = parser.parse_args(argv)
    try:
        if args.command == "stable":
            return stable(args.out, args.publish)
        if args.command == "next":
            publish_next(args.product, args.sha, args.dist)
        elif args.command == "next-sha":
            print(next_sha(args.product))
        else:
            return fixtures(args.check)
    except FAILURES as error:
        print(f"error: {describe(error)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
