# Optional bounded mutation scans, independent of ordinary Rust test gates.

[positional-arguments]
[group('mutation')]
[doc('Bounded Rust mutation scan; arguments pass through to the runner.')]
mutation-rust *args:
    bun tools/main.ts mutation-run "$@"

# The runner's exact cargo-mutants, checkout-local under target/mutation-tools.
[group('mutation')]
mutation-rust-install:
    bun tools/main.ts mutation-install
