# The desktop host is the standalone GPUI executable
# clients/desktop/native/target/{debug,release}/phux-desktop.
# `desktop-source` bootstraps the parent-managed GPUI snapshot.
[group('desktop')]
desktop-source:
    python3 clients/desktop/toolchain/bootstrap.py

[private]
desktop-host-debug: desktop-source
    bash clients/desktop/scripts/build-host.sh --debug

[group('desktop')]
[doc('Build the release GPUI desktop from the pinned snapshot')]
desktop-source-build: desktop-native-build

[group('desktop')]
[doc('Install the desktop script toolchain from the frozen package lock')]
desktop-install:
    cd clients/desktop && bun install --frozen-lockfile

[group('desktop')]
[doc('Check script tooling, qualification guards and the desktop host lock')]
desktop-check: worktree-claim
    cd clients/desktop && bun run check:tooling
    python3 scripts/check-desktop-host-lock.py
    python3 scripts/test-check-desktop-host-lock.py

[group('desktop')]
[doc('Run the native desktop crate tests (model, painter, input, presentation, launch)')]
desktop-test: worktree-claim desktop-source
    cargo test --locked --manifest-path clients/desktop/native/Cargo.toml

[group('desktop')]
[doc('Measure GPUI terminal draws headlessly (pass release for optimized comparisons)')]
desktop-perf-headless profile="test": desktop-source
    cargo test --locked --manifest-path clients/desktop/native/Cargo.toml --profile {{profile}} --test pixels -- --headless-perf

[group('desktop')]
[doc('Format check and clippy -D warnings for the native desktop crate')]
desktop-lint: worktree-claim desktop-source
    cargo fmt --manifest-path clients/desktop/native/Cargo.toml -- --check
    cargo clippy --locked --manifest-path clients/desktop/native/Cargo.toml --all-targets -- -D warnings

[group('desktop')]
[doc('Repair repairable desktop host lock drift without resolving GPUI')]
desktop-lock-fix:
    python3 scripts/check-desktop-host-lock.py --fix

[group('desktop')]
[doc('Re-resolve the desktop host lock against the pinned GPUI snapshot')]
desktop-lock-refresh: desktop-source
    cargo metadata --manifest-path clients/desktop/native/Cargo.toml --format-version 1 >/dev/null

[group('desktop')]
[doc('Build the phux CLI from this checkout for the desktop launcher')]
desktop-cli:
    bash clients/desktop/scripts/build-cli.sh

[group('desktop')]
[doc('Build the debug GPUI desktop and launch it against this checkout')]
desktop-app: worktree-claim desktop-host-debug desktop-cli
    cd clients/desktop && PHUX_BIN="${CARGO_TARGET_DIR:-{{ justfile_directory() }}/target}/debug/phux" PHUX_DESKTOP_BIN="$PWD/native/target/debug/phux-desktop" bun scripts/launch-desktop.ts

[group('desktop')]
[doc('Opt-in desktop demo with a checkout-built CLI and disposable private server')]
desktop-demo: desktop-host-debug desktop-cli
    # mbx may place target/debug/phux outside the checkout. Materialize the
    # checkout-built binary locally so the demo's provenance guard can verify it.
    mkdir -p clients/desktop/.cache/demo-target/debug
    cp -L "${CARGO_TARGET_DIR:-{{ justfile_directory() }}/target}/debug/phux" clients/desktop/.cache/demo-target/debug/phux
    cd clients/desktop && CARGO_TARGET_DIR="$PWD/.cache/demo-target" bun scripts/desktop-demo.ts

[group('desktop')]
[doc('Package the desktop and qualify it against an isolated same-checkout server')]
desktop-framework-check: desktop-package desktop-cli
    #!/usr/bin/env bash
    set -euo pipefail
    phux="${CARGO_TARGET_DIR:-{{ justfile_directory() }}/target}/debug/phux"
    if [[ "$(uname -s)" == Linux ]]; then
        xvfb-run -a -s '-screen 0 1920x1080x24' dbus-run-session -- bun clients/desktop/scripts/smoke-linux.ts --app clients/desktop/dist/linux/phux-desktop --phux "$phux"
    else
        bun clients/desktop/scripts/smoke-app.ts --app clients/desktop/dist/app/Phux.app --phux "$phux"
    fi

[group('desktop')]
[doc('Build the release GPUI desktop executable')]
desktop-native-build: worktree-claim desktop-source
    bash clients/desktop/scripts/build-host.sh --release

[group('desktop')]
[doc('Qualify the packaged app: presented evidence, OCR, and session survival')]
desktop-native-test: desktop-framework-check

[group('desktop')]
[doc('Package the release desktop for the native platform under clients/desktop/dist')]
desktop-package: desktop-native-build
    #!/usr/bin/env bash
    set -euo pipefail
    cd clients/desktop
    if [[ "$(uname -s)" == Linux ]]; then
        bun scripts/package-linux.ts
    else
        bun scripts/package-app.ts
    fi

[group('desktop')]
[doc('Build Phux.app and install it to /Applications; it opens your Intake session')]
desktop-install-app: desktop-native-build
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" == Linux ]]; then
        echo 'Linux desktop installation uses signed release bundles; run curl -fsSL https://phux.sh/install-desktop | PHUX_REQUIRE_SIGNATURE=1 sh' >&2
        exit 2
    fi
    cd clients/desktop
    bun scripts/package-app.ts --install

[group('desktop')]
[doc('Run the headless GPUI benchmark and fail on scene, time or RSS budget regressions')]
desktop-perf-budget output="clients/desktop/native/target/perf-headless": desktop-source
    mkdir -p "{{output}}"
    cargo test --locked --manifest-path clients/desktop/native/Cargo.toml --test pixels -- --headless-perf > "{{output}}/headless.jsonl"
    bun clients/desktop/scripts/perf-budget.ts "{{output}}/headless.jsonl"
