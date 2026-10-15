# Optional bounded mutation scans. Not a PR gate.


# Optional bounded mutation scans, independent of ordinary Rust test gates.
[positional-arguments]
[group('mutation')]
[doc('Bounded Rust mutation scan; arguments pass through to the runner.')]
mutation-rust *args:
    bash scripts/mutation/rust.sh "$@"


# Real-tool adapter acceptance in disposable fixtures (requires the pinned tool).
[group('mutation')]
mutation-rust-check:
    python3 scripts/mutation/rust_check.py
