#!/usr/bin/env python3
"""Test the pinned winit cursor helper itself, without dependency dev-deps."""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import tomllib


CLIENT = Path(__file__).resolve().parents[1]
MODULE = Path("src/platform_impl/linux/wayland/window/ime_cursor.rs")
REQUIRED_TESTS = frozenset(
    {
        "tests::first_publication_is_not_suppressed",
        "tests::unrelated_frames_preserve_the_published_rectangle",
        "tests::each_position_and_size_component_changes_the_key",
        "tests::dpi_changes_refresh_an_identical_protocol_rectangle",
        "tests::context_reset_republishes_the_same_rectangle_once",
        "tests::windows_do_not_share_publication_state",
    }
)


def pinned_source(manifest):
    patch = manifest["patch"]["crates-io"]["winit"]
    revision = patch.get("rev", "")
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("winit patch must pin an immutable full 40-character revision")
    git = patch.get("git", "")
    if not git:
        raise ValueError("winit patch must specify its git source")
    return f"git+{git}?rev={revision}#{revision}"


def pinned_package(metadata, source):
    packages = [
        package for package in metadata["packages"] if package["name"] == "winit"
    ]
    if len(packages) != 1:
        raise ValueError("expected exactly one resolved winit package")
    package = packages[0]
    if package.get("source") != source:
        raise ValueError(
            "resolved winit source does not match the manifest's exact git/rev"
        )
    return package


def cursor_module(metadata, source):
    package = pinned_package(metadata, source)
    module = Path(package["manifest_path"]).parent / MODULE
    for required in (module, module.with_name("ime_cursor_tests.rs")):
        if not required.is_file():
            raise ValueError(f"pinned winit is missing {required.name}")
    return module


def verify_listing(listing):
    names = {
        line.removesuffix(": test")
        for line in listing.splitlines()
        if line.endswith(": test")
    }
    if names != REQUIRED_TESTS:
        missing = sorted(REQUIRED_TESTS - names)
        extra = sorted(names - REQUIRED_TESTS)
        raise ValueError(
            f"expected six cursor regressions; missing={missing}, extra={extra}"
        )


def run_tests(module):
    # Compile in place: the helper's #[path] loads its real sibling tests.
    # Only the executable is temporary; Cargo's checkout remains untouched.
    with tempfile.TemporaryDirectory(
        prefix="wayland-cursor-", dir=os.environ.get("TMPDIR")
    ) as temp:
        binary = Path(temp) / "wayland-cursor-tests"
        subprocess.run(
            [
                "rustc",
                "--edition=2021",
                "--test",
                "-Dwarnings",
                str(module),
                "-o",
                str(binary),
            ],
            check=True,
        )
        listing = subprocess.run(
            [str(binary), "--list"],
            check=True,
            capture_output=True,
            text=True,
        )
        verify_listing(listing.stdout)
        subprocess.run([str(binary), "--test-threads=1"], check=True)


def main():
    with (CLIENT / "Cargo.toml").open("rb") as file:
        source = pinned_source(tomllib.load(file))
    metadata = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=CLIENT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    run_tests(cursor_module(json.loads(metadata.stdout), source))


if __name__ == "__main__":
    main()
