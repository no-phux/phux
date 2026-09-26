# Optional bounded mutation scans. Not a PR gate.


# Optional bounded mutation scans; arguments are passed to the language runner.
# These are independent of cockpit-test and the ordinary Rust test gates.
[positional-arguments]
[group('mutation')]
[doc('Bounded Zig mutation scan; arguments pass through to the runner.')]
mutation-zig *args:
    bash scripts/mutation/zig.sh "$@"


[positional-arguments]
[group('mutation')]
[doc('Bounded Rust mutation scan; arguments pass through to the runner.')]
mutation-rust *args:
    bash scripts/mutation/rust.sh "$@"


# Real-tool adapter acceptance in disposable fixtures (requires the pinned tool).
[group('mutation')]
mutation-rust-check:
    python3 scripts/mutation/rust_check.py


[group('mutation')]
[doc('Zig mutation adapter acceptance against disposable fixtures.')]
mutation-zig-check:
    PHUX_ZIG_MUTATION_INTEGRATION=1 python3 -B -m unittest discover -s scripts/mutation -p test_zig.py -v
