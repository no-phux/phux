#!/usr/bin/env python3
"""Build and publish the per-channel release manifest (ADR-0175).

`stable` and `next` derive the manifest from what GitHub has published, never
from what a run intended, so a cancelled or failed run leaves the previous
manifest correct and the next run repairs it. `publish` uploads it to the
`channels` prerelease only when its content changed. `fixtures` rewrites the
golden manifests every client's tests read.

The rendered form is the contract the POSIX installers parse without a JSON
library: two-space indentation, one member per line, no arrays, and string
values restricted to SAFE.
"""

import argparse
import datetime
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = "no-phux/phux"
DOWNLOAD = f"https://github.com/{REPO}/releases/download"
CHANNELS_RELEASE = "channels"
CHANNELS = ("stable", "next")
PHUX_TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
)
DESKTOP_TARGET = "aarch64-apple-darwin"
ROOT = Path(__file__).resolve().parents[2]
FIXTURES = ROOT / "scripts/fixtures/release-manifest"

SAFE = re.compile(r"^[A-Za-z0-9._:/+-]*$")
VERSION = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
SHA = re.compile(r"^[0-9a-f]{40}$")
DIGEST = re.compile(r"^[0-9a-f]{64}$")
PROTOCOL = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
PROTOCOL_SOURCE = re.compile(
    r"pub const PROTOCOL_VERSION: Version = Version \{\s*major: (\d+),\s*minor: (\d+),"
)
STABLE_TAG = re.compile(r"^(desktop-)?v((0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*))$")


class ManifestError(Exception):
    pass


def require(pattern, value, what):
    if not isinstance(value, str) or not pattern.fullmatch(value):
        raise ManifestError(f"{what} is not valid: {value!r}")
    return value


def release_of(product, channel, version):
    if channel == "next":
        return "next"
    return f"v{version}" if product == "phux" else f"desktop-v{version}"


def artifact_name(product, channel, version, sha, target):
    identity = f"next.{sha}" if channel == "next" else None
    if product == "phux":
        return f"phux-{identity or 'v' + version}-{target}.tar.gz"
    return f"phux-desktop-{identity or version}-macos-arm64.zip"


def product_entry(product, channel, version, sha, protocol, digests, minimum_macos=None):
    """One product's manifest entry, with every derived field filled in."""
    require(VERSION, version, f"{product} version")
    require(SHA, sha, f"{product} sha")
    require(PROTOCOL, protocol, f"{product} protocol")
    targets = PHUX_TARGETS if product == "phux" else (DESKTOP_TARGET,)
    if set(digests) != set(targets):
        raise ManifestError(f"{product} needs digests for exactly {', '.join(targets)}")
    release = release_of(product, channel, version)
    artifacts = {}
    for target in targets:
        name = artifact_name(product, channel, version, sha, target)
        artifacts[target] = {
            "name": name,
            "url": f"{DOWNLOAD}/{release}/{name}",
            "sha256": require(DIGEST, digests[target], f"{name} sha256"),
        }
    entry = {"version": version, "release": release, "sha": sha, "protocol": protocol}
    if product == "desktop":
        entry["minimum_macos"] = require(PROTOCOL, minimum_macos, "desktop minimum_macos")
    entry["artifacts"] = artifacts
    return entry


def build(channel, products, generated_at):
    if channel not in CHANNELS:
        raise ManifestError(f"unknown channel {channel!r}")
    if not set(products) <= {"phux", "desktop"}:
        raise ManifestError(f"unknown products: {sorted(set(products) - {'phux', 'desktop'})}")
    ordered = {name: products[name] for name in ("phux", "desktop") if name in products}
    return {
        "schema_version": 1,
        "channel": channel,
        "generated_at": generated_at,
        "products": ordered,
    }


def render(manifest):
    def check(value, path):
        if isinstance(value, dict):
            if not value:
                raise ManifestError(f"{path} is an empty object")
            for key, child in value.items():
                require(re.compile(r"^[A-Za-z0-9_-]+$"), key, f"key at {path}")
                check(child, f"{path}.{key}")
        elif isinstance(value, bool) or not isinstance(value, (str, int)):
            raise ManifestError(f"{path} has unsupported type {type(value).__name__}")
        elif isinstance(value, str):
            require(SAFE, value, path)

    check(manifest, "manifest")
    return json.dumps(manifest, indent=2) + "\n"


def same_content(left, right):
    strip = lambda doc: {k: v for k, v in doc.items() if k != "generated_at"}
    return strip(left) == strip(right)


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# --- published state --------------------------------------------------------


def run(*args, **kwargs):
    return subprocess.run(args, check=True, text=True, capture_output=True, **kwargs).stdout


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
    source = run("git", "show", f"{ref}:crates/phux-protocol/src/lib.rs")
    match = PROTOCOL_SOURCE.search(source)
    if not match:
        raise ManifestError(f"PROTOCOL_VERSION not found at {ref}")
    return f"{int(match.group(1))}.{int(match.group(2))}"


def download(tag, name, into):
    run("gh", "release", "download", tag, "--repo", REPO, "--pattern", name,
        "--dir", str(into), "--clobber")
    return (Path(into) / name).read_text()


def sidecar_digest(text, name):
    """The digest a `sha256sum`-format file publishes for `name`."""
    for line in text.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].lstrip("*") == name:
            return require(DIGEST, parts[0], f"{name} sha256")
    raise ManifestError(f"no digest for {name}")


def newest_stable(rows):
    """The newest published tag per product from `[tag, draft, prerelease]` rows."""
    newest = {}
    for tag, draft, prerelease in rows:
        match = STABLE_TAG.fullmatch(tag)
        if draft or prerelease or not match:
            continue
        product = "desktop" if match.group(1) else "phux"
        version = tuple(int(part) for part in match.group(2).split("."))
        if product not in newest or version > newest[product][0]:
            newest[product] = (version, match.group(2), tag)
    return {product: (version, tag) for product, (_, version, tag) in newest.items()}


FAILURES = (ManifestError, subprocess.CalledProcessError, json.JSONDecodeError,
            KeyError, OSError)


def describe(error):
    detail = getattr(error, "stderr", None) or ""
    return f"{error} {detail}".strip()


def release_assets(tag):
    """Asset names on `tag`, or None when the release does not exist. Any
    other failure raises: an unreadable release is not an unpublished one."""
    view = subprocess.run(["gh", "release", "view", tag, "--repo", REPO, "--json", "assets",
                           "--jq", ".assets[].name"], capture_output=True, text=True)
    if view.returncode != 0:
        if "release not found" in view.stderr.lower():
            return None
        raise subprocess.CalledProcessError(view.returncode, view.args, view.stdout, view.stderr)
    return set(view.stdout.split())


def gather(builders):
    """Run each product's builder. A builder returns None when the product is
    not published; a failure is recorded, never mistaken for absence."""
    products, failures = {}, {}
    for product, builder in builders:
        try:
            entry = builder()
        except FAILURES as error:
            failures[product] = describe(error)
            continue
        if entry is not None:
            products[product] = entry
    return products, failures


def stable_phux(newest, scratch):
    if "phux" not in newest:
        return None
    version, tag = newest["phux"]
    digests = {}
    for target in PHUX_TARGETS:
        name = artifact_name("phux", "stable", version, None, target)
        digests[target] = sidecar_digest(download(tag, f"{name}.sha256", scratch), name)
    sha = tag_commit(tag)
    return product_entry("phux", "stable", version, sha, protocol_at(tag, "phux"), digests)


def stable_desktop(newest, scratch):
    if "desktop" not in newest:
        return None
    version, tag = newest["desktop"]
    name = artifact_name("desktop", "stable", version, None, DESKTOP_TARGET)
    digest = sidecar_digest(download(tag, "SHA256SUMS", scratch), name)
    record = json.loads(download(tag, "desktop-channel.json", scratch))
    if record.get("sha256") != digest:
        raise ManifestError(f"{tag} desktop-channel.json disagrees with SHA256SUMS")
    sha = tag_commit(tag)
    return product_entry("desktop", "stable", version, sha, protocol_at(tag, "desktop"),
                         {DESKTOP_TARGET: digest}, record.get("minimum_macos"))


def collect_stable(scratch):
    rows = run("gh", "api", "--paginate", f"repos/{REPO}/releases?per_page=100",
               "--jq", ".[] | [.tag_name, .draft, .prerelease] | @json")
    newest = newest_stable(json.loads(line) for line in rows.splitlines() if line)
    return gather([
        ("phux", lambda: stable_phux(newest, scratch)),
        ("desktop", lambda: stable_desktop(newest, scratch)),
    ])


def next_phux(assets, scratch):
    if "channel.json" not in assets:
        return None
    record = json.loads(download("next", "channel.json", scratch))
    sha = require(SHA, record.get("sha"), "next channel.json sha")
    digests = {}
    for target in PHUX_TARGETS:
        name = artifact_name("phux", "next", None, sha, target)
        digests[target] = sidecar_digest(download("next", f"{name}.sha256", scratch), name)
    return product_entry("phux", "next", record.get("version"), sha,
                         protocol_at(sha, "phux"), digests)


def next_desktop(assets, scratch):
    if "desktop-channel.json" not in assets:
        return None
    record = json.loads(download("next", "desktop-channel.json", scratch))
    sha = require(SHA, record.get("sha"), "next desktop-channel.json sha")
    name = artifact_name("desktop", "next", None, sha, DESKTOP_TARGET)
    digest = sidecar_digest(download("next", f"{name}.sha256", scratch), name)
    if record.get("sha256") != digest:
        raise ManifestError("next desktop-channel.json disagrees with its sidecar")
    return product_entry("desktop", "next", record.get("version"), sha,
                         protocol_at(sha, "desktop"), {DESKTOP_TARGET: digest},
                         record.get("minimum_macos"))


def collect_next(scratch):
    """Next is built from its per-product publication records (ADR-0113)."""
    assets = release_assets("next") or set()
    return gather([
        ("phux", lambda: next_phux(assets, scratch)),
        ("desktop", lambda: next_desktop(assets, scratch)),
    ])


def published_manifest(channel):
    """The manifest clients see now, or None when there is none yet."""
    assets = release_assets(CHANNELS_RELEASE)
    if not assets or f"{channel}.json" not in assets:
        return None
    with tempfile.TemporaryDirectory() as scratch:
        return json.loads(download(CHANNELS_RELEASE, f"{channel}.json", scratch))


def carry_forward(products, failures, previous):
    """A product that failed to resolve keeps its published entry, so one
    broken lane cannot drop or freeze another's release."""
    kept = dict(products)
    previous_products = (previous or {}).get("products", {})
    for product in failures:
        if product in previous_products:
            kept[product] = previous_products[product]
    return {name: kept[name] for name in ("phux", "desktop") if name in kept}


def ensure_channels_release():
    if release_assets(CHANNELS_RELEASE) is not None:
        return
    create = subprocess.run(
        ["gh", "release", "create", CHANNELS_RELEASE, "--repo", REPO, "--prerelease",
         "--latest=false", "--title", CHANNELS_RELEASE, "--notes",
         "Release manifests (ADR-0175): stable.json and next.json. Not a release."],
        capture_output=True, text=True)
    # The other channel's workflow may have created it first.
    if create.returncode != 0 and release_assets(CHANNELS_RELEASE) is None:
        raise subprocess.CalledProcessError(create.returncode, create.args,
                                            create.stdout, create.stderr)


def publish(path):
    manifest = json.loads(Path(path).read_text())
    channel = manifest["channel"]
    name = f"{channel}.json"
    if Path(path).name != name:
        raise ManifestError(f"upload {name} under its own basename, not {Path(path).name}")
    ensure_channels_release()
    current = published_manifest(channel)
    if current is not None and same_content(current, manifest):
        print(f"{name} is already current")
        return
    run("gh", "release", "upload", CHANNELS_RELEASE, str(path), "--repo", REPO, "--clobber")
    print(f"published {name}")


def build_channel(channel, out, collect):
    """Write `out` from published state; return the products that failed."""
    with tempfile.TemporaryDirectory() as scratch:
        products, failures = collect(scratch)
    if failures:
        products = carry_forward(products, failures, published_manifest(channel))
    for product, detail in failures.items():
        print(f"error: {channel} {product}: {detail}", file=sys.stderr)
    if not products:
        raise ManifestError(f"nothing is published on {channel}")
    Path(out).write_text(render(build(channel, products, now())))
    print(f"wrote {out}")
    return failures


# --- golden fixtures --------------------------------------------------------


def fixture(channel):
    """Deterministic manifests every client's tests parse (one golden per channel)."""
    phux_sha, desktop_sha = "a" * 40, "b" * 40
    digests = {target: f"{index:x}" * 64 for index, target in enumerate(PHUX_TARGETS, 1)}
    products = {
        "phux": product_entry("phux", channel, "9.8.7", phux_sha, "0.9", digests),
        "desktop": product_entry("desktop", channel, "1.2.3", desktop_sha, "0.9",
                                 {DESKTOP_TARGET: "d" * 64}, "27.0"),
    }
    return build(channel, products, "2026-01-01T00:00:00Z")


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)
    for channel in CHANNELS:
        command = commands.add_parser(channel, help=f"build {channel}.json from published state")
        command.add_argument("--out", required=True)
        command.add_argument("--publish", action="store_true",
                             help="upload it, then fail if any product could not be resolved")
    command = commands.add_parser("publish", help="upload a manifest when its content changed")
    command.add_argument("path")
    command = commands.add_parser("fixtures", help="rewrite or check the golden fixtures")
    command.add_argument("--check", action="store_true")
    args = parser.parse_args(argv)
    try:
        if args.command == "publish":
            publish(args.path)
        elif args.command == "fixtures":
            stale = []
            for channel in CHANNELS:
                path = FIXTURES / f"{channel}.json"
                text = render(fixture(channel))
                if args.check:
                    if not path.exists() or path.read_text() != text:
                        stale.append(str(path.relative_to(ROOT)))
                else:
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(text)
            if stale:
                raise ManifestError(
                    f"stale: {', '.join(stale)}; run python3 scripts/ci/release_manifest.py fixtures")
        else:
            collect = collect_stable if args.command == "stable" else collect_next
            failures = build_channel(args.command, args.out, collect)
            if args.publish:
                publish(args.out)
            if failures:
                # The manifest kept those products' previous entries; the red
                # job is what says a lane needs attention.
                return 1
    except FAILURES as error:
        print(f"error: {describe(error)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
