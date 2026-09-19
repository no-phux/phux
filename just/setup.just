# Contributor setup: doctor, native smoke, toolchain pins.
# Native CI uses the rustup/zig helpers; humans use mise or nix.


# Check prerequisites for only this work area; also works without just via bash.
[group('setup')]
doctor SCOPE="native":
    bash scripts/doctor.sh "{{SCOPE}}"


# Native contributor smoke: real compiler/linker/VT build and codec tests, no Nix.
[group('setup')]
native-smoke:
    bash scripts/native-smoke.sh


# Setup helper behavior, including missing tools and rejected compiler downloads.
[group('setup')]
[private]
setup-check:
    bash scripts/test-dev-setup.sh


# Scaffold a commented starter config into a worktree-local XDG dir
# (./.phux-xdg) so you can test config changes without touching your real
# ~/.config/phux. Re-run freely: `phux config init` refuses to clobber.
# Inspect the result with: XDG_CONFIG_HOME="$PWD/.phux-xdg" phux config show
[group('setup')]
[doc('Scaffold a starter config into a worktree-local XDG dir (./.phux-xdg).')]
scaffold-config:
    XDG_CONFIG_HOME="{{justfile_directory()}}/.phux-xdg" cargo run -q -p phux -- config init


# Rust's native manifest, verified Zig manifest, Mise, the Nix flake, CI,
# standalone WASM workspaces, and pinned container builders agree on their
# toolchains. Static: reads files, so it runs on a CI checkout with no Nix, no
# Mise and no compilers, which is why it can be a `ci` gate.
[group('setup')]
[doc('Toolchain pins agree across manifests, Mise, Nix, CI, and containers.')]
toolchain-check:
    bash scripts/check-toolchain-sync.sh


# Runtime half of toolchain-check: both environments, actual binaries.
# Not in `ci` (no Mise on a CI checkout). Skips if either env is missing.
[group('setup')]
toolchain-parity:
    bash scripts/check-toolchain-parity.sh


# Print the toolchain we are pinned to.
[group('setup')]
toolchain:
    @rustc --version
    @cargo --version
