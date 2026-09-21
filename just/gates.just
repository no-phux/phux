# Root gates. `just ci` / `just ci-full` live here so changing composition
# routes the Rust lanes without firing Cockpit, web, or native-setup.


# Small pure-Rust loop: no Zig, Node, nextest, or browser tools.
[group('gates')]
core-check: (doctor "core") (crate-check "phux-core")


# One crate's gate; optional test features (e.g. phux-protocol server).
[group('gates')]
crate-check PACKAGE FEATURES="":
    #!/usr/bin/env bash
    set -euo pipefail
    args=(--locked -p "{{PACKAGE}}")
    if [[ -n "{{FEATURES}}" ]]; then args+=(--features "{{FEATURES}}"); fi
    cargo fmt --all -- --check
    cargo clippy "${args[@]}" --all-targets -- -D warnings
    RUSTDOCFLAGS='-D warnings' cargo doc "${args[@]}" --no-deps
    {{AUTO_SPAWN_BACKSTOP}} cargo test "${args[@]}"


# The binding crate's opt-in UniFFI encoder (ADR-0135). `lint` and `doc` are
# already --all-features, but the unit pool builds default features only, so
# this lane's tests would otherwise stop running the day the two shims merged.
# It is not a default feature precisely so Cockpit's archive never links it.
# --no-default-features --lib: the mobile artifact never carries the c-abi
# lane (see the build scripts), and its own ~200 tests already run in the
# default-features unit pool, so this only needs to prove the lib target
# (projection/ plus uniffi/) builds and passes with c-abi off.
[group('gates')]
[private]
binding-uniffi-test:
    {{AUTO_SPAWN_BACKSTOP}} cargo test --locked -p phux-client-ffi --no-default-features --features uniffi --lib


# One agent integration's type, unit, packed-artifact, and audit gates.
[group('gates')]
integration-check PACKAGE:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{PACKAGE}}" in runtime|opencode|pi|claude) ;; *) echo 'choose runtime, opencode, pi, or claude' >&2; exit 2 ;; esac
    bash scripts/doctor.sh integrations
    # Incidental install audits are off; the explicit audit in gates still runs.
    export npm_config_audit=false npm_config_fund=false
    npm --prefix "integrations/{{PACKAGE}}" ci
    npm --prefix "integrations/{{PACKAGE}}" run gates


# Format every Rust file in place.
[group('gates')]
fmt:
    cargo fmt --all


# CI-style format check — fails if anything is dirty.
[group('gates')]
[private]
fmt-check:
    cargo fmt --all -- --check


# Clippy with warnings denied. The bar.
[group('gates')]
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings


# Inner loop with the same flags `ci` uses. Includes e2e because CI's test
# job does; `just ci` does not. No backticks in the echo — just would run them.
[group('gates')]
[doc('Iteration loop: the fast gates ci would reject you on, in the order ci hits them.')]
precommit: fmt lint docs-gen test e2e
    @echo "precommit clean - now run 'just ci' once to confirm the remaining gates"


# Lint shell scripts with shellcheck (the harness, the boundary/docs
# guards, and the examples). Provided by the dev shell. Gates at
# `warning` severity: the examples carry deliberate `info`-level nits
# (sourced libs shellcheck can't follow, single-quoted heredoc-ish
# program strings) that are correct as written. On-demand, not in `ci`.
[group('gates')]
[doc('Lint the harness, guard, and example shell scripts at warning severity.')]
shellcheck:
    shellcheck --severity=warning scripts/*.sh scripts/ci/*.sh \
      examples/agents/*.sh \
      examples/agents/orchestrate-placed-fleet examples/agents/tests/*.sh \
      examples/plugins/*/scripts/*.sh


# GitHub workflow + composite-action syntax, the fail-closed CI path-routing
# truth table, and the SHA-pin policy for action references.
[group('gates')]
[private]
[doc('Workflow syntax, the CI path-routing truth table, and action SHA pins.')]
workflow-check:
    actionlint .github/workflows/*.yml
    bash scripts/check-release-cpu-baselines.sh
    python3 clients/cockpit/scripts/check-build-contracts.py
    python3 scripts/mutation/test_recipes.py
    bash scripts/test-dev-setup.sh
    node --test scripts/test-vt-wasm.mjs
    bash scripts/ci/check-classify-changes.sh
    python3 -B -m unittest discover -s scripts/ci -p 'test_*.py'
    bash scripts/check-action-pins.sh
    node scripts/check-release-orchestration.mjs
    node scripts/check-release-drift-policy.mjs


# Dependency hygiene: licenses, advisories, bans.
[group('gates')]
deny:
    cargo deny check


# Default, lean and headless production dependency boundaries (compile-free).
[group('gates')]
[private]
build-features-check:
    python3 scripts/check-build-features.py


# Build rustdoc with warnings denied — mirrors the CI `doc` gate.
[group('gates')]
doc:
    RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features


# Doc system gates: frontmatter, TL;DR, dead links, ADR status, spec version.
# See docs/CONVENTIONS.md.
[group('gates')]
[doc('Doc gates: frontmatter, TL;DR, dead links, ADR status, spec version.')]
docs-check:
    bash scripts/check-docs.sh


# Regenerate the generated reference docs (docs/reference/) from the compiled
# binary. Run after any change to the CLI surface: a freshness unit test in
# crates/phux/src/refdocs/ byte-compares the tree against this generator's
# output on every `just test`, so a stale tree fails CI with this recipe as
# the remedy. Idempotent. See docs/CONVENTIONS.md §"Generated reference docs".
[group('gates')]
[doc('Regenerate docs/reference/ from the compiled binary; run after CLI changes.')]
docs-gen:
    cargo run -q -p phux --bin phux -- gen-reference-docs


# Homebrew formula generator vs the shapes release.yml's matrix can produce.
[group('gates')]
[private]
formula-check:
    bash scripts/check-formula.sh


# release.yml's pinned Zig tarball digests vs ziglang.org's published index.
# Skips (exit 0) when the index is unreachable, so it is safe offline.
[group('gates')]
[private]
[doc('Pinned Zig tarball digests vs the published ziglang.org index.')]
zig-pin-check:
    bash scripts/check-zig-pins.sh


# Install/release documentation contracts (README, INSTALL, RELEASING, the
# installer, the formula generator, release.yml). Also run by
# `just release-preflight`; in `ci` so it cannot rot between releases.
[group('gates')]
[private]
[doc('Install and release doc contracts: README, INSTALL, RELEASING, installer.')]
install-surface-check:
    bash scripts/check-install-surface.sh


# Every released agent-facing binary embeds a configless, EPIPE-safe --skill.
[group('gates')]
[private]
skill-contract:
    cargo build -p phux -p phux-mcp
    bash scripts/check-skill-contract.sh


# Generated glyph table vs its .bdf source — catches hand edits and stale regens.
[group('gates')]
[private]
font-check:
    bash scripts/check-generated-font.sh


# Every #[ignore]d e2e binary is named by some lane — no test rots unrun.
[group('gates')]
[private]
e2e-lane-check:
    bash scripts/check-e2e-lanes.sh


# All integration packages and their shared version contract.
[group('gates')]
[private]
agent-integrations-check:
    #!/usr/bin/env bash
    set -euo pipefail
    node scripts/check-agent-integration-versions.mjs
    just integration-check runtime
    git diff --exit-code -- integrations/runtime/dist
    for package in opencode pi claude; do
      just integration-check "$package"
    done


# Full root gate set; iterate with scoped checks from docs/SETUP.md first.
# Keep independent gates ahead of tests: a flaky test must not hide rustdoc or
# contract failures. CONTRIBUTING.md owns the local/CI gate map (phux-yb1m).
[group('gates')]
[doc('Full root gate set: the deterministic and unit bar.')]
ci: fmt-check lint doc deny build-features-check build-features-compile docs-check workflow-check formula-check font-check e2e-lane-check zig-pin-check toolchain-check install-surface-check skill-contract agent-integrations-check binding-uniffi-test test
    @echo "ok"


# Full root PR bar, including timing-sensitive e2e and agent example smoke.
# Browser and Cockpit clients have separate gates; see docs/SETUP.md.
[group('gates')]
[doc('Full root PR bar: ci plus the real-server e2e and agent smoke lanes.')]
ci-full: ci e2e agents-fleet-smoke
    @echo "ok (full)"
