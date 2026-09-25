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
    python3 clients/desktop/scripts/test-bootstrap-source.py

[group('desktop')]
[doc('Run the pinned upstream Solid native GPU and consumer suite')]
desktop-source-test:
    cd clients/desktop/toolchain/gpuix/packages/solid && bun run test

[group('desktop')]
[doc('Launch the Solid desktop against an existing daemon socket')]
desktop-app: desktop-native-build
    cd clients/desktop && PHUX_DESKTOP_ADDON="$PWD/.cache/host/phux-desktop-native.darwin-arm64.node" bun scripts/launch-desktop.ts

[group('desktop')]
[doc('Build and drive a production Solid framework window using the source-built addon')]
desktop-framework-check:
    cmp clients/desktop/toolchain/gpuix/packages/native/index.d.ts clients/desktop/node_modules/@gpuix/native/index.d.ts
    diff -qr clients/desktop/toolchain/gpuix/packages/solid/dist clients/desktop/node_modules/@gpuix/solid/dist
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
[doc('Prove the wrapper shares the FFI Client using an isolated real PTY server')]
desktop-native-client-test:
    CARGO_TARGET_DIR="$PWD/clients/desktop/.cache/client-test-target" cargo run --locked -p phux-client-ffi --no-default-features --features napi --example napi_smoke -- "$PWD/clients/desktop/.cache/host/phux-desktop-native.darwin-arm64.node" --production-host

[group('desktop')]
[doc('Exercise independent terminal views through the combined native addon')]
desktop-native-view-test:
    CARGO_TARGET_DIR="$PWD/clients/desktop/.cache/client-test-target" cargo run --locked -p phux-client-ffi --no-default-features --features napi --example napi_view_smoke -- "$PWD/clients/desktop/.cache/host/phux-desktop-native.darwin-arm64.node"

[group('desktop')]
[doc('Build the separate native terminal acceptance addon with fixture exports')]
desktop-native-fixtures-build:
    bash clients/desktop/scripts/build-host.sh --terminal-fixtures

[group('desktop')]
[doc('Paint real PTY publications through Metal using the acceptance addon')]
desktop-native-painter-test:
    PHUX_DESKTOP_ADDON="$PWD/clients/desktop/.cache/host-fixtures/phux-desktop-native.darwin-arm64.node" bash clients/desktop/tests/native/terminal-run.sh
