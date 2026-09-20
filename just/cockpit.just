# Native macOS Cockpit. Changing this file routes cockpit-ci only.


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
cockpit-test: cockpit-ffi cockpit-build-contracts
    ./scripts/check-release-version.sh
    ./scripts/check-sdk-pin.sh
    ./scripts/lib/zon_test.sh
    ./scripts/lib/measure_test.sh
    ./scripts/generate-operation-fixtures_test.sh
    ./scripts/zig-build.sh test -Dplatform=null -Dphux-enabled=true -Dphux-client-ffi-profile=ffi-dev --summary all


# Optional full run of the default app graph (DisabledPhuxProvider). CI
# typechecks that graph inside `just cockpit-test` instead of re-running tests.
[group('cockpit')]
[doc('Cockpit tests with the app graph built without the Phux provider.')]
[working-directory('clients/cockpit')]
cockpit-test-no-phux: cockpit-ffi
    ./scripts/zig-build.sh test -Dplatform=null -Dphux-client-ffi-profile=ffi-dev --summary all


# Node TypeScript tests. The loader strips @native-sdk/core's published .ts
# (plain `node --test` on Node 24 will not). Needs clients/cockpit/node_modules.
[group('cockpit')]
[doc('Cockpit Node TypeScript tests via navigation-loader.')]
[working-directory('clients/cockpit')]
cockpit-node-test:
    node --import ./src/tests/navigation-loader.mjs --test ./src/tests/*.test.mjs


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
