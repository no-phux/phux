# Native macOS Cockpit. Frozen (ADR-0167): these recipes are local only.
# Changing this file does not start Cockpit CI. Resume: clients/cockpit/FROZEN.md.


# Build the stable C ABI and the native macOS Cockpit from this checkout.
[group('cockpit')]
[working-directory('clients/cockpit')]
cockpit-build: cockpit-artifacts
    bash ./scripts/build-shipping-app.sh -Dphux-client-ffi-profile=ffi-dev --summary all


# Run the shipping TypeScript graph, native engine regressions, and repository
# and release contract checks.
[group('cockpit')]
[doc('Cockpit TypeScript graph, native engine regressions, and contract checks.')]
[working-directory('clients/cockpit')]
cockpit-test: cockpit-ffi cockpit-ffi-abi cockpit-build-contracts cockpit-node-test
    ./scripts/check-release-version.sh
    ./scripts/check-sdk-pin.sh
    ./scripts/check-sdk-pin_test.sh
    ./scripts/dev-run_test.sh
    ./scripts/lib/worktree-owner_test.sh
    ./scripts/lib/zon_test.sh
    ./scripts/lib/measure_test.sh
    ./scripts/generate-operation-fixtures_test.sh
    # The TypeScript lifecycle test execs this checkout's CLI and stops that
    # server. CARGO_BIN_EXE_phux is the same name the Rust integration tests
    # read; PATH is not a substitute.
    bash ./scripts/build-phux-artifacts.sh --only cli ffi-dev
    # PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE is the server's own backstop: an open
    # connection disarms it, and a daemon this process failed to reap still exits.
    CARGO_BIN_EXE_phux="$(CDPATH='' cd -- ../.. && pwd)/target/ffi-dev/phux" \
        PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE=30 \
        ./scripts/zig-build.sh test -Dplatform=null -Dphux-enabled=true -Dphux-client-ffi-profile=ffi-dev --summary all


# Optional full run of the default app graph (DisabledPhuxProvider). CI
# typechecks that graph inside `just cockpit-test` instead of re-running tests.
[group('cockpit')]
[doc('Cockpit tests with the app graph built without the Phux provider.')]
[working-directory('clients/cockpit')]
cockpit-test-no-phux: cockpit-ffi
    ./scripts/zig-build.sh test -Dplatform=null -Dphux-client-ffi-profile=ffi-dev --summary all


# Node's default TypeScript stripping cannot load @native-sdk/core from
# node_modules. Keep install and the loader in the canonical shared helper.
[group('cockpit')]
[doc('Cockpit Node TypeScript tests via navigation-loader.')]
[working-directory('clients/cockpit')]
cockpit-node-test:
    ./scripts/cockpit-node-test.sh


# Split out of cockpit-test only because it is the one step that runs from the
# repository root; the rest share [working-directory('clients/cockpit')].
[group('cockpit')]
[private]
cockpit-build-contracts:
    python3 clients/cockpit/scripts/check-build-contracts.py


# Build only Cockpit's static archive, with fast unwind-safe development codegen.
[group('cockpit')]
cockpit-ffi:
    cargo rustc --locked --profile ffi-dev -p phux-client-ffi --lib --crate-type staticlib


# Compile, link, and run the C ABI checks (crates/phux-client-ffi/tests/*_abi.c)
# against the header and the static archive cockpit-ffi built: the layouts
# Cockpit compiles against, pinned by static asserts.
[group('cockpit')]
[doc('Compile, link, and run the C ABI checks against the cockpit-ffi archive.')]
cockpit-ffi-abi: cockpit-ffi
    bash crates/phux-client-ffi/tests/run_abi_checks.sh ffi-dev


# Production static FFI archive; release codegen with the required panic boundary.
[group('cockpit')]
cockpit-ffi-release:
    cargo rustc --locked --profile ffi-release -p phux-client-ffi --lib --crate-type staticlib


# Run the isolated developer app with the Phux-backed production graph.
[group('cockpit')]
[working-directory('clients/cockpit')]
cockpit-dev: cockpit-artifacts
    ./scripts/dev-run.sh --phux --ffi-profile ffi-dev


# The FFI + header artifacts both app recipes need, built from the repository
# root before either drops into clients/cockpit.
[group('cockpit')]
[private]
cockpit-artifacts:
    bash clients/cockpit/scripts/build-phux-artifacts.sh ffi-dev
