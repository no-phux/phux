# Matched upstream source is built separately from the application's TS7 checks.
[group('desktop')]
desktop-source:
    python3 clients/desktop/scripts/bootstrap-source.py

[group('desktop')]
[doc('Build the pinned release native GPUIX addon and Solid adapter')]
desktop-source-build: desktop-source
    bash clients/desktop/scripts/build-source.sh

[group('desktop')]
[doc('Install the desktop TypeScript toolchain from the frozen package lock')]
desktop-install:
    cd clients/desktop && bun install --frozen-lockfile

[group('desktop')]
[doc('Check native TS7, Oxc, Solid rules and source-pin guards (not native app acceptance)')]
desktop-check:
    cd clients/desktop && bun run check:tooling
    cd clients/desktop && bun test tests/desktop-demo.test.ts
    python3 clients/desktop/scripts/test-bootstrap-source.py
    python3 scripts/check-desktop-host-lock.py
    python3 scripts/test-check-desktop-host-lock.py

[group('desktop')]
[doc('Repair repairable desktop host lock drift without bootstrapped GPUIX')]
desktop-lock-fix:
    python3 scripts/check-desktop-host-lock.py --fix

[group('desktop')]
[doc('Re-resolve the desktop host lock against the pinned GPUIX source')]
desktop-lock-refresh: desktop-source
    cargo metadata --manifest-path clients/desktop/native/Cargo.toml --format-version 1 >/dev/null

[group('desktop')]
[doc('Run the pinned upstream Solid native GPU and consumer suite')]
desktop-source-test:
    cd clients/desktop/toolchain/gpuix/packages/solid && bun run test

[group('desktop')]
[doc('Build the phux CLI from this checkout for the desktop launcher')]
desktop-cli:
    bash clients/desktop/scripts/build-cli.sh

[group('desktop')]
[doc('Build the desktop from pinned GPUIX source and launch it')]
desktop-app: desktop-install desktop-native-build desktop-cli
    cd clients/desktop && PHUX_BIN="${CARGO_TARGET_DIR:-{{ justfile_directory() }}/target}/debug/phux" PHUX_DESKTOP_ADDON="$PWD/.cache/host/phux-desktop-native.darwin-arm64.node" bun scripts/launch-desktop.ts

[group('desktop')]
[doc('Opt-in desktop demo with a checkout-built CLI/addon and disposable private server')]
desktop-demo: desktop-install desktop-native-build desktop-cli
    cd clients/desktop && bun scripts/desktop-demo.ts

[group('desktop')]
[doc('Build and drive a production Solid framework window using the source-built addon')]
desktop-framework-check: desktop-source-build
    # The bundle verifies and uses patched source, not the unpatched npm artifacts.
    cd clients/desktop && bun scripts/check-framework.ts

[group('desktop')]
[doc('Build the single native desktop addon and check generated declarations')]
desktop-native-build: desktop-source
    bash clients/desktop/scripts/build-host.sh

[group('desktop')]
[doc('Exercise native extension lifecycle and addon identity on Metal')]
desktop-native-test:
    bash clients/desktop/tests/native/run-smoke.sh

[group('desktop')]
[doc('Prove the wrapper shares the FFI Client, then fault-inject input loss and restart, on isolated real PTY servers')]
desktop-native-client-test:
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/lib/dev-toolchain.sh
    check_desktop_toolchain
    export CARGO_TARGET_DIR="$PWD/clients/desktop/.cache/client-test-target"
    cargo run --locked -p phux-client-ffi --no-default-features --features napi --example napi_smoke -- "$PWD/clients/desktop/.cache/host/phux-desktop-native.darwin-arm64.node" --production-host
    # The fault cases need the napi_host fixture's NativeClientLease export,
    # which the production wrapper deliberately does not ship.
    cargo build --locked -p phux-client-ffi --no-default-features --features napi --example napi_host
    cp "$CARGO_TARGET_DIR/debug/examples/libnapi_host.dylib" "$CARGO_TARGET_DIR/debug/examples/napi_host.node"
    cargo run --locked -p phux-client-ffi --no-default-features --features napi --example napi_smoke -- "$CARGO_TARGET_DIR/debug/examples/napi_host.node" --faults

[group('desktop')]
[doc('Exercise independent terminal views through the combined native addon')]
desktop-native-view-test:
    #!/usr/bin/env bash
    set -euo pipefail
    source scripts/lib/dev-toolchain.sh
    check_desktop_toolchain
    CARGO_TARGET_DIR="$PWD/clients/desktop/.cache/client-test-target" cargo run --locked -p phux-client-ffi --no-default-features --features napi --example napi_view_smoke -- "$PWD/clients/desktop/.cache/host/phux-desktop-native.darwin-arm64.node"

[group('desktop')]
[doc('Build the separate native terminal acceptance addon with fixture exports')]
desktop-native-fixtures-build:
    bash clients/desktop/scripts/build-host.sh --terminal-fixtures

[group('desktop')]
[doc('Paint real PTY publications through Metal using the acceptance addon')]
desktop-native-painter-test:
    PHUX_DESKTOP_ADDON="$PWD/clients/desktop/.cache/host-fixtures/phux-desktop-native.darwin-arm64.node" bash clients/desktop/tests/native/terminal-run.sh

[group('desktop')]
[doc('Build Phux.app (one compiled executable plus the release addon) under clients/desktop/dist/app')]
desktop-package: desktop-install desktop-native-build
    cd clients/desktop && bun scripts/package-app.ts

[group('desktop')]
[doc('Build Phux.app and install it to /Applications; it attaches your default server session')]
desktop-install-app: desktop-install desktop-native-build
    cd clients/desktop && bun scripts/package-app.ts --install
