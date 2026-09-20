# Developer builds and install-dev/rebuild.


# Quick type-check across the workspace.
[group('build')]
check:
    cargo check --workspace --all-targets


# Build the developer executables (debug).
[group('build')]
build:
    cargo build --locked -p phux -p phux-mcp


# Build every workspace target, including test harnesses, examples and benches.
[group('build')]
build-all:
    cargo build --workspace --all-targets


# Build without browser HTTP/3 support; UDS, WebSocket and raw QUIC remain.
[group('build')]
build-lean:
    cargo build --locked -p phux -p phux-mcp --no-default-features


# Release executables with full LTO.
[group('build')]
build-release:
    cargo build --locked -p phux -p phux-mcp --release


# Build the current checkout and atomically install its developer binaries.
# The binaries live in Cargo's bin dir, matching normal source installs. Keep
# that directory ahead of Homebrew in PATH so there is one developer binary.
[group('build')]
[doc('Build and atomically install the developer binaries into Cargo bin dir.')]
install-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -p phux -p phux-mcp
    install_dir="${CARGO_HOME:-$HOME/.cargo}/bin"
    mkdir -p "$install_dir"
    install -m 755 target/debug/phux "$install_dir/.phux.new"
    install -m 755 target/debug/phux-mcp "$install_dir/.phux-mcp.new"
    mv -f "$install_dir/.phux.new" "$install_dir/phux"
    mv -f "$install_dir/.phux-mcp.new" "$install_dir/phux-mcp"
    echo "installed development binaries to $install_dir"
    echo "phux -> $install_dir/phux"


# Install the rebuilt developer binaries, then hot-swap a server that was
# already started from the source-install path, preserving sessions (ADR-0032).
# A server originally started by Homebrew needs a one-time restart first.
[group('build')]
[doc('Install rebuilt binaries, then hot-swap the running server, keeping sessions.')]
rebuild:
    just install-dev
    "${CARGO_HOME:-$HOME/.cargo}/bin/phux" upgrade


# Compile opt-out consumers separately: workspace feature unification masks them.
[group('build')]
[private]
build-features-compile:
    cargo check --locked -p phux-client -p phux-mcp --all-targets --no-default-features --features phux-client/testkit
    cargo check --locked -p phux --no-default-features --bin phux
    cargo check --locked -p phux-tui --all-targets --no-default-features


# Watch loop — re-check + test on every save.
[group('build')]
watch:
    cargo watch -x check -x 'nextest run --workspace'
