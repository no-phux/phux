#!/usr/bin/env python3
"""Test the pinned wgpu dependency in a disposable standalone Cargo workspace.

Run on Linux with Vulkan/GL (lavapipe is sufficient):
  mise exec -- python3 clients/desktop/toolchain/test-wgpu.py
  CARGO_TARGET_DIR=/path/to/isolated/target python3 .../test-wgpu.py wgpu_renderer::tests

Additional arguments are forwarded to `cargo test --workspace --lib --features test-support`.
No committed lockfile, materialized source, or installed application is modified.
"""

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

TOOLCHAIN = Path(__file__).resolve().parent
# These are the original fixtures named by the inherited GPUI unit tests.
# Pin their immutable Zed revision: gpui-fast's archive omits fonts and metadata.
FONT_REVISION = "279fe070bb389b79652e52065b2f001edcc0b11b"
FONTS = (
    "lilex/Lilex-Regular.ttf",
    "lilex/Lilex-Bold.ttf",
    "ibm-plex-sans/IBMPlexSans-Regular.ttf",
)


def fetch_fonts(workspace: Path) -> None:
    for relative in FONTS:
        target = workspace / "assets" / "fonts" / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        url = f"https://raw.githubusercontent.com/zed-industries/zed/{FONT_REVISION}/assets/fonts/{relative}"
        with urllib.request.urlopen(url, timeout=60) as response, target.open("wb") as output:
            shutil.copyfileobj(response, output)


def prepare_workspace(workspace: Path) -> None:
    shutil.copytree(
        TOOLCHAIN / "gpui" / "gpui-fast-wgpu",
        workspace / "gpui" / "gpui-fast-wgpu",
        ignore=shutil.ignore_patterns(".git", "target", "Cargo.lock"),
    )
    gpui = json.dumps(str(TOOLCHAIN / "gpui" / "gpui-fast"))
    (workspace / "Cargo.toml").write_text(
        '[workspace]\nresolver = "3"\nmembers = ["gpui/gpui-fast-wgpu"]\n\n'
        f"[patch.crates-io]\ngpui-fast = {{ path = {gpui} }}\n"
    )
    # Reuse the desktop's resolved versions. Cargo may prune unrelated host
    # packages or add test-only entries in this disposable copy, never in native/.
    shutil.copy2(TOOLCHAIN.parent / "native" / "Cargo.lock", workspace / "Cargo.lock")
    # gpui-fast's shared rounded-clip tests retain their upstream sibling path.
    fixture = Path("src/fast/rounded_clip_fixtures.rs")
    fixture_target = workspace / "gpui" / "gpui" / fixture
    fixture_target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(TOOLCHAIN / "gpui" / "gpui-fast" / fixture, fixture_target)
    # Upstream unit tests reference omitted archive fixtures three levels above
    # src/. Keep that layout entirely inside the disposable workspace.
    fetch_fonts(workspace)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    _, cargo_arguments = parser.parse_known_args()
    subprocess.run([sys.executable, str(TOOLCHAIN / "bootstrap.py")], check=True)
    with tempfile.TemporaryDirectory(prefix="phux-wgpu-tests-") as scratch:
        workspace = Path(scratch)
        prepare_workspace(workspace)
        return subprocess.run(
            ["cargo", "test", "--workspace", "--lib", "--features", "test-support", *cargo_arguments],
            cwd=workspace,
            check=False,
        ).returncode


if __name__ == "__main__":
    sys.exit(main())
