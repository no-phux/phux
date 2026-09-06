#!/usr/bin/env python3
"""Check production dependency boundaries without compiling or resolving dev deps."""

import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def dependency_features(*selection):
    """Read Cargo's target-specific, feature-unified production graph."""
    output = subprocess.check_output(
        [
            "cargo", "tree", "--locked", "--edges", "normal,build",
            "--prefix", "none", "--format", "{p}|{f}", *selection,
        ],
        cwd=ROOT,
        text=True,
    )
    rows = (
        line.removesuffix(" (*)").split("|", 1)
        for line in output.splitlines() if line
    )
    return {package.split()[0]: set(features.split(",")) for package, features in rows}


def main():
    full = dependency_features("-p", "phux")
    lean = dependency_features("-p", "phux", "-p", "phux-mcp", "--no-default-features")
    mcp = dependency_features("-p", "phux-mcp")
    assert "wtransport" in full, "the default executable must retain browser HTTP/3"
    assert "layout-cache" not in full["ratatui-core"], "unused layout cache re-enabled"
    assert "wtransport" not in lean, "the lean executable must not inherit server defaults"
    assert "ratatui" not in mcp, "headless MCP must not compile TUI chrome"
    for name, graph in (("full", full), ("lean", lean), ("mcp", mcp)):
        assert "ratatui-crossterm" not in graph, f"{name}: unused terminal backend"
        assert "ratatui-macros" not in graph, f"{name}: unused TUI macros"
        assert "tls12" not in graph["rustls"], f"{name}: TLS 1.2 re-enabled"
        print(f"{name}: {len(graph)} distinct package names; feature boundaries OK")


if __name__ == "__main__":
    main()
