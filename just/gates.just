# Gates. Each ci.yml lane job runs one recipe from here or a sibling file;
# `just check` runs the lanes your diff needs through those same recipes
# (tools/src/routing/plan.ts maps lanes to recipes). There is no run-everything
# recipe: route, then run what the route says.


# Run the ci.yml lanes this checkout's changes need, against origin/main.
[group('gates')]
[doc('Run exactly the CI lanes your changes need, through the same recipes CI runs.')]
check *ARGS: worktree-claim
    bun tools/main.ts check {{ARGS}}


# Small pure-Rust loop: no Zig, Node, nextest, or browser tools.
[group('gates')]
core-check: (doctor "core") (crate-check "phux-core")


# One crate's gate; optional test features (e.g. phux-protocol server).
[group('gates')]
crate-check PACKAGE FEATURES="": worktree-claim
    #!/usr/bin/env bash
    set -euo pipefail
    args=(--locked -p "{{PACKAGE}}")
    if [[ -n "{{FEATURES}}" ]]; then args+=(--features "{{FEATURES}}"); fi
    cargo fmt --all -- --check
    cargo clippy "${args[@]}" --all-targets -- -D warnings
    RUSTDOCFLAGS='-D warnings' cargo doc "${args[@]}" --no-deps
    {{AUTO_SPAWN_BACKSTOP}} cargo test "${args[@]}"


# The `guards` lane: compile-free repository contracts, on every change.
[group('gates')]
[doc('Compile-free contracts: workflows, shell, tools, docs, installers, pins.')]
guards: brand-check shellcheck tools-check docs-check install-surface-check
    actionlint .github/workflows/*.yml
    bash scripts/check-action-pins.sh
    bash scripts/check-file-tokens_test.sh
    bash scripts/check-toolchain-sync.sh
    bash scripts/check-cache-portable.sh
    bash scripts/check-e2e-lanes.sh
    bash scripts/check-release-cpu-baselines.sh
    bash scripts/test-demo-pi-setup.sh
    bash scripts/lib/worktree-owner_test.sh
    bash scripts/check-file-tokens_test.sh
    node --test scripts/test-npm-audit-gate.mjs
    bash examples/agents/tests/placed-fleet-smoke.sh
    bash examples/agents/tests/program-status-smoke.sh
    python3 -B -m unittest discover -s distros/crew -p 'test_*.py'
    bun tools/main.ts desktop-host-lock


# The `lint` lane. Clippy and rustdoc see all features; the compile checks
# catch opt-out builds that workspace feature unification masks.
[group('gates')]
[doc('Rust format, clippy, rustdoc, deny, and feature-boundary checks.')]
rust-lint: worktree-claim fmt-check lint doc deny
    bun tools/main.ts build-features-check
    cargo check --locked -p phux-client -p phux-mcp --all-targets --no-default-features --features phux-client/testkit
    cargo check --locked -p phux --no-default-features --bin phux
    cargo check --locked -p phux-tui --all-targets --no-default-features
    bash scripts/check-ghostty-pin.sh


# The binding crate's opt-in UniFFI encoder (ADR-0135). The unit pool builds
# default features only, and the mobile artifact never carries the c-abi lane,
# so this proves the lib target with c-abi off.
[group('gates')]
[private]
binding-uniffi-test:
    {{AUTO_SPAWN_BACKSTOP}} cargo test --locked -p phux-client-ffi --no-default-features --features uniffi --lib


# One agent integration's type, unit, packed-artifact, and audit gates.
[group('gates')]
integration-check PACKAGE: worktree-claim
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{PACKAGE}}" in runtime|opencode-v2|omp|pi|claude|grok) ;; *) echo 'choose runtime, opencode-v2, omp, pi, claude, or grok' >&2; exit 2 ;; esac
    bash scripts/doctor.sh integrations
    export npm_config_audit=false npm_config_fund=false
    case "{{PACKAGE}}" in
        omp|opencode-v2) cd "integrations/{{PACKAGE}}"; bun install --frozen-lockfile; bun run gates ;;
        grok) node --test integrations/grok/test/package.test.mjs ;;
        pi)
            npm --prefix integrations/runtime ci
            npm --prefix integrations/runtime run build
            npm --prefix integrations/pi ci
            npm --prefix integrations/pi run gates
            ;;
        *) npm --prefix "integrations/{{PACKAGE}}" ci; npm --prefix "integrations/{{PACKAGE}}" run gates ;;
    esac


# The `integrations` lane: every package, the runtime dist, and npm audit.
[group('gates')]
[doc('Every agent integration package, the shared runtime, and its dist check.')]
agent-integrations-check:
    bash scripts/ci/agent-integrations.sh


# Format every Rust file in place.
[group('gates')]
fmt: worktree-claim
    cargo fmt --all


[group('gates')]
[private]
fmt-check:
    cargo fmt --all -- --check


# Clippy with warnings denied. The bar.
[group('gates')]
lint: worktree-claim
    cargo clippy --workspace --all-targets --all-features -- -D warnings


# Build rustdoc with warnings denied.
[group('gates')]
doc:
    RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features


# Dependency hygiene: licenses, advisories, bans.
[group('gates')]
deny:
    cargo deny check


# Shell scripts at warning severity: the examples carry deliberate info-level
# nits (sourced libs shellcheck cannot follow) that are correct as written.
[group('gates')]
shellcheck:
    shellcheck --severity=warning scripts/*.sh scripts/lib/*.sh scripts/ci/*.sh \
      examples/agents/*.sh \
      examples/agents/orchestrate-placed-fleet examples/agents/tests/*.sh \
      examples/plugins/*/scripts/*.sh


# The tools package (build and CI logic, ADR-0184): format, types, lint, tests.
# Its tests hold the routing table, the lane plan, and the workflow policy.
[group('gates')]
tools-check:
    cd tools && bun install --frozen-lockfile && bun run check


# Doc gates: frontmatter, TL;DR, dead links, ADR status, spec version.
[group('gates')]
docs-check:
    bash scripts/check-docs.sh
    bash scripts/check-docs_test.sh


# Regenerate docs/reference/ from the compiled binary after CLI changes; a
# freshness unit test fails `just test` with this recipe as the remedy.
[group('gates')]
docs-gen:
    cargo run -q -p phux --bin phux -- gen-reference-docs


[group('gates')]
[private]
install-surface-check:
    bash scripts/check-install-surface.sh


# Lease this worktree to the running agent session, or refuse (exit 3) when
# another live session holds it (AGENTS.md "Worktree Isolation"). Human shells
# and CI are never checked. scripts/lib/worktree-owner_test.sh holds the list
# of recipes that depend on it.
[private]
worktree-claim:
    @bash scripts/worktree-claim.sh


# After a long run (`test`, `e2e`): fail (exit 3) when HEAD moved or the lease
# changed hands since the claim, so a straddled run is not a green.
[private]
worktree-verify CALLER:
    @bash scripts/worktree-verify.sh "{{CALLER}}"


# Drop this agent session's worktree lease when handing the worktree on.
[group('setup')]
[doc("Release this agent session's worktree lease for the next session.")]
worktree-release:
    @bash scripts/worktree-release.sh worktree-release

# Verify committed branding without accessing the private source library.
[group('gates')]
brand-check:
    python3 -B scripts/brand.py check
    python3 -B -m unittest discover -s scripts -p 'test_brand*.py'

# Install an explicitly checksummed approved release (or roll back to one).
[positional-arguments]
brand-sync ARCHIVE SHA256:
    python3 -B scripts/brand.py sync --archive "$1" --sha256 "$2"

# Restore the current release pin from the public brand archive.
brand-restore:
    python3 -B scripts/brand.py restore
