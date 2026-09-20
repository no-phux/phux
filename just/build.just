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
# Same destination as `curl | sh` (`$HOME/.local/bin`, or `$PHUX_INSTALL_DIR`)
# so PATH does not have to be rearranged. Also publish to Cargo's bin dir so
# a cargo-first PATH still hits this checkout.
[group('build')]
[doc('Build and atomically install the developer binaries next to the curl installer.')]
install-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -p phux -p phux-mcp
    publish() {
        local dest=$1
        mkdir -p "$dest"
        # Replace a symlink with a real binary so we do not write through
        # to another install location.
        [[ -L "$dest/phux" ]] && rm -f "$dest/phux"
        [[ -L "$dest/phux-mcp" ]] && rm -f "$dest/phux-mcp"
        install -m 755 target/debug/phux "$dest/.phux.new"
        install -m 755 target/debug/phux-mcp "$dest/.phux-mcp.new"
        mv -f "$dest/.phux.new" "$dest/phux"
        mv -f "$dest/.phux-mcp.new" "$dest/phux-mcp"
        echo "phux -> $dest/phux"
    }
    user_dir="${PHUX_INSTALL_DIR:-$HOME/.local/bin}"
    cargo_dir="${CARGO_HOME:-$HOME/.cargo}/bin"
    publish "$user_dir"
    if [[ "$(cd "$user_dir" && pwd -P)" != "$(cd "$cargo_dir" && pwd -P)" ]]; then
        publish "$cargo_dir"
    fi


# Install the rebuilt developer binaries, then hot-swap a server that was
# already started from the source-install path, preserving sessions (ADR-0032).
# A server originally started by Homebrew needs a one-time restart first.
[group('build')]
[doc('Install rebuilt binaries, then hot-swap the running server, keeping sessions.')]
rebuild:
    just install-dev
    phux upgrade


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
