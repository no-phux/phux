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


# One crate's gate; optional test features (e.g. phux-vt kitty).
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
    bun tools/main.ts design-gen --check
    bun tools/main.ts file-tokens-check --merge-base origin/main
    bash packaging/test-release-cpu-baselines.sh
    bash dev/lib/worktree-owner_test.sh
    bash dev/worktree-prune_test.sh
    bash examples/agents/tests/placed-fleet-smoke.sh
    bash examples/agents/tests/program-status-smoke.sh
    python3 -B examples/agents/tests/test-agent-examples.py
    python3 -B -m unittest discover -s distros/crew -p 'test_*.py'


# The `lint` lane. Clippy and rustdoc see all features; the compile checks
# catch opt-out builds that workspace feature unification masks.
[group('gates')]
[doc('Rust format, clippy, rustdoc, deny, and feature-boundary checks.')]
rust-lint: worktree-claim fmt-check lint doc deny
    bun tools/main.ts build-features-check
    cargo check --locked -p phux-client -p phux-mcp --all-targets --no-default-features --features phux-client/testkit
    cargo check --locked -p phux --no-default-features --bin phux
    cargo check --locked -p phux-tui --all-targets --no-default-features
    bun tools/main.ts ghostty-pin


# The binding crate with the mobile artifact's exact features (ADR-0135).
[group('gates')]
[private]
binding-uniffi-test:
    {{AUTO_SPAWN_BACKSTOP}} cargo test --locked -p phux-client-ffi --no-default-features --features uniffi --lib


# One agent integration's type, unit, packed-artifact, and audit gates.
[group('gates')]
integration-check PACKAGE: worktree-claim
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{PACKAGE}}" in runtime|opencode-v2|omp|pi|claude|grok|muse) ;; *) echo 'choose runtime, opencode-v2, omp, pi, claude, grok, or muse' >&2; exit 2 ;; esac
    bash dev/doctor.sh integrations
    # Incidental install audits are off; the explicit audit inside each
    # package's gates script still runs.
    export npm_config_audit=false npm_config_fund=false
    case "{{PACKAGE}}" in
        omp) cd integrations/omp; bun install --frozen-lockfile; bun run gates ;;
        opencode-v2)
            cd integrations/opencode-v2
            bun install --frozen-lockfile
            bun run gates
            # index.js and tui.js ship as committed bundles; the build must not change them.
            git diff --exit-code -- .
            ;;
        grok) node --test integrations/grok/test/package.test.mjs ;;
        muse) npm --prefix integrations/muse run gates ;;
        pi)
            npm --prefix integrations/runtime ci
            npm --prefix integrations/runtime run build
            npm --prefix integrations/pi ci
            npm --prefix integrations/pi run gates
            ;;
        *) npm --prefix "integrations/{{PACKAGE}}" ci; npm --prefix "integrations/{{PACKAGE}}" run gates ;;
    esac


# The `integrations` lane: every package, the shared runtime, and the voice bridge.
[group('gates')]
[doc('Every agent integration, including the shared runtime and the Hex iOS bridge.')]
agent-integrations-check: (integration-check "runtime") (integration-check "opencode-v2") (integration-check "omp") (integration-check "pi") (integration-check "claude") (integration-check "grok") (integration-check "muse")
    python3 -B -m unittest discover -s integrations/hex-ios


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
    shellcheck --severity=warning dev/*.sh dev/lib/*.sh install/*.sh install/lib/*.sh packaging/*.sh packaging/skills/*.sh scripts/*.sh \
      clients/rune/upstream/*.sh clients/phux-vt-web/scripts/*.sh \
      examples/agents/*.sh \
      examples/agents/orchestrate-placed-fleet examples/agents/record-pi-live-fleet examples/agents/tests/*.sh \
      plugins/*/scripts/*.sh benchmarks/*/*.sh


# The tools package (build and CI logic, ADR-0184): format, types, lint, tests.
# Its tests hold the routing table, the lane plan, and the workflow policy.
[group('gates')]
tools-check:
    cd tools && bun install --frozen-lockfile && bun run check


# Check documentation links, commands, crate maps, code citations, and llms.txt.
# Its fixtures are tools/src/docs/*.test.ts, which tools-check runs.
[group('gates')]
docs-check *ARGS:
    bun tools/main.ts docs-check {{ARGS}}


# Regenerate docs/reference/ from the compiled binary after CLI changes.
[group('gates')]
[doc('Regenerate reference pages from the CLI, config, and action catalogs.')]
docs-gen:
    cargo run -q -p phux --bin phux -- gen-reference-docs


# Regenerate the design-token exports after editing crates/phux-design/tokens/src
# (`--check` refuses drift; guards runs it).
[group('gates')]
[doc('Regenerate the design-token exports from crates/phux-design/tokens/src.')]
design-gen *ARGS:
    bun tools/main.ts design-gen {{ARGS}}


[group('gates')]
[private]
install-surface-check:
    bash install/test.sh


# Lease this worktree to the running agent session, or refuse (exit 3) when
# another live session holds it (AGENTS.md "Worktree Isolation"). Human shells
# and CI are never checked. dev/lib/worktree-owner_test.sh holds the list
# of recipes that depend on it.
[private]
worktree-claim:
    @bash dev/worktree-lease.sh claim


# After a long run (`test`, `e2e`): fail (exit 3) when HEAD moved or the lease
# changed hands since the claim, so a straddled run is not a green.
[private]
worktree-verify CALLER:
    @bash dev/worktree-lease.sh verify "{{CALLER}}"


# Drop this agent session's worktree lease when handing the worktree on.
[group('setup')]
[doc("Release this agent session's worktree lease for the next session.")]
worktree-release:
    @bash dev/worktree-lease.sh release worktree-release


# Reclaim disk from idle linked worktrees (dev/worktree-prune.sh): remove
# clean ones whose pushed branch is merged, prune build output from the rest.
[group('setup')]
[doc('Prune idle worktrees (dry run; --apply acts, --outputs-only keeps every worktree).')]
worktree-prune *ARGS:
    @bash dev/worktree-prune.sh {{ARGS}}

# Verify committed branding without accessing the private source library.
[group('gates')]
brand-check:
    python3 -B brand/brand.py check
    python3 -B -m unittest discover -s brand -p 'test_brand*.py'

# Install an explicitly checksummed approved release (or roll back to one).
[positional-arguments]
brand-sync ARCHIVE SHA256:
    python3 -B brand/brand.py sync --archive "$1" --sha256 "$2"

# Restore the current release pin from the public brand archive.
brand-restore:
    python3 -B brand/brand.py restore
