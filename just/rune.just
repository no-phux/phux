# The Rune client (clients/rune). Its workspace and lock are its own, so these
# are the only recipes that reach it; rune-build.yml runs the same lines.
[group('rune')]
[doc('Format check, clippy, tests and release-rule tests for the Rune client')]
rune-check: worktree-claim
    bash clients/rune/scripts/check.sh

[group('rune')]
[doc("Advisories, licenses and sources over Rune's own lockfile")]
rune-deny:
    cargo-deny --manifest-path clients/rune/Cargo.toml --config clients/rune/deny.toml --locked check
