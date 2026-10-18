# Root gates. `just ci` / `just ci-full` live here so changing composition
# routes the Rust lanes without firing web or native-setup.


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


# The binding crate's opt-in UniFFI encoder (ADR-0135). `lint` and `doc` are
# already --all-features, but the unit pool builds default features only, so
# this lane's tests would otherwise stop running the day the two shims merged.
# It is not a default feature so C ABI archives never link it implicitly.
# --no-default-features --lib: the mobile artifact never carries the c-abi
# lane (see the build scripts), and its own ~200 tests already run in the
# default-features unit pool, so this only needs to prove the lib target
# (projection/ plus uniffi/) builds and passes with c-abi off.
[group('gates')]
[private]
binding-uniffi-test:
    {{AUTO_SPAWN_BACKSTOP}} cargo test --locked -p phux-client-ffi --no-default-features --features uniffi --lib


# One agent integration's type, unit, packed-artifact, and audit gates.
# The all-packages lane also checks generated artifact freshness.
[group('gates')]
integration-check PACKAGE: worktree-claim
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{PACKAGE}}" in runtime|opencode-v2|omp|pi|claude|grok) ;; *) echo 'choose runtime, opencode-v2, omp, pi, claude, or grok' >&2; exit 2 ;; esac
    bash scripts/doctor.sh integrations
    # Incidental install audits are off; the explicit audit in gates still runs.
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


# Format every Rust file in place.
[group('gates')]
fmt: worktree-claim
    cargo fmt --all


# CI-style format check — fails if anything is dirty.
[group('gates')]
[private]
fmt-check:
    cargo fmt --all -- --check


# Clippy with warnings denied. The bar.
[group('gates')]
lint: worktree-claim
    cargo clippy --workspace --all-targets --all-features -- -D warnings


# Inner loop with the same flags `ci` uses. Includes e2e because CI's test
# job does; `just ci` does not. No backticks in the echo — just would run them.
[group('gates')]
[doc('Iteration loop: the fast gates ci would reject you on, in the order ci hits them.')]
precommit: worktree-claim fmt lint docs-gen test e2e
    @. ./scripts/lib/output.sh; phux_ok "precommit clean - now run 'just ci' once to confirm the remaining gates"


# Lint shell scripts with shellcheck (the harness, the boundary/docs
# guards, scripts/ci, and the examples). Provided by the dev shell. Gates
# at `warning` severity: the examples carry deliberate `info`-level nits
# (sourced libs shellcheck can't follow, single-quoted heredoc-ish
# program strings) that are correct as written. `workflow-check` depends
# on this recipe, so `just ci` and ci.yml's workflow gate both fail if
# these scripts rot.
[group('gates')]
[doc('Lint harness, CI, guard, and example shell scripts at warning severity.')]
shellcheck:
    shellcheck --severity=warning scripts/*.sh scripts/lib/*.sh scripts/ci/*.sh \
      examples/agents/*.sh \
      examples/agents/orchestrate-placed-fleet examples/agents/tests/*.sh \
      examples/plugins/*/scripts/*.sh


# The tools package (build and CI logic, ADR-0184): format, types, lint, tests.
[group('gates')]
[doc('Format, typecheck, lint and test the build and CI tools package.')]
tools-check:
    cd tools && bun install --frozen-lockfile && bun run check


# GitHub workflow + composite-action syntax, the fail-closed CI path-routing
# truth table, and the SHA-pin policy for action references.
[group('gates')]
[private]
[doc('Workflow syntax, the CI path-routing truth table, and action SHA pins.')]
workflow-check: shellcheck tools-check
    actionlint .github/workflows/*.yml
    bash scripts/check-release-cpu-baselines.sh
    bash scripts/test-dev-setup.sh
    bash scripts/test-demo-pi-setup.sh
    bash scripts/lib/worktree-owner_test.sh
    node --test scripts/test-vt-wasm.mjs
    node --test scripts/test-npm-audit-gate.mjs
    bash scripts/check-action-pins.sh
    bun tools/main.ts release-orchestration-check


# Dependency hygiene: licenses, advisories, bans.
[group('gates')]
deny:
    cargo deny check


# Default, lean and headless production dependency boundaries (compile-free).
[group('gates')]
[private]
build-features-check:
    bun tools/main.ts build-features-check


# Build rustdoc with warnings denied — mirrors the CI `doc` gate.
[group('gates')]
doc:
    RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features


# Dead links, dead anchors, and `phux`/`just` commands in docs that no
# longer exist.
[group('gates')]
[doc('Docs gate: dead links and anchors, stale phux and just commands.')]
docs-check:
    bash scripts/check-docs.sh


# Regenerate the generated reference docs (docs/reference/) from the compiled
# binary. Run after any change to the CLI surface: a freshness unit test in
# crates/phux/src/refdocs/ byte-compares the tree against this generator's
# output on every `just test`, so a stale tree fails CI with this recipe as
# the remedy. Idempotent.
[group('gates')]
[doc('Regenerate docs/reference/ from the compiled binary; run after CLI changes.')]
docs-gen:
    cargo run -q -p phux --bin phux -- gen-reference-docs


# release.yml's pinned Zig tarball digests vs ziglang.org's published index.
# Skips (exit 0) when the index is unreachable, so it is safe offline.
[group('gates')]
[private]
[doc('Pinned Zig tarball digests vs the published ziglang.org index.')]
zig-pin-check:
    bash scripts/check-zig-pins.sh


# flake.nix's Ghostty input vs the commit the pinned libghostty-rs builds.
# Skips (exit 0) when GitHub is unreachable, so it is safe offline.
[group('gates')]
[private]
[doc('Flake Ghostty input vs the pinned libghostty-rs GHOSTTY_COMMIT.')]
ghostty-pin-check:
    bash scripts/check-ghostty-pin.sh


# No env!("CARGO_MANIFEST_DIR") / env!("CARGO_BIN_EXE_*") in library or test
# code: a compiled-in checkout path defeats the shared build cache.
[group('gates')]
[private]
[doc('No compile-time checkout paths in library or test code.')]
cache-portable-check:
    bash scripts/check-cache-portable.sh


[group('gates')]
[private]
[doc('Installer behaviour and the install surface contracts.')]
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
    bun tools/main.ts gen-bitmap-font --check


# Every #[ignore]d e2e binary is named by some lane — no test rots unrun.
[group('gates')]
[private]
e2e-lane-check:
    bash scripts/check-e2e-lanes.sh


# All integration packages and their shared version contract.
# The body is scripts/ci/agent-integrations.sh so ci.yml runs the same
# commands as this recipe, including the runtime suite and dist check.
[group('gates')]
[private]
agent-integrations-check:
    bash scripts/ci/agent-integrations.sh


# Lease this worktree to the running agent session, or refuse (exit 3) when
# another live session holds it (AGENTS.md "Worktree Isolation"). Human shells
# and CI are never checked. Every recipe that builds, tests or formats the tree
# depends on it; scripts/lib/worktree-owner_test.sh holds the list.
[private]
worktree-claim:
    @bash scripts/worktree-claim.sh


# After a long run (`ci`, `test`, `e2e`): fail (exit 3) when HEAD moved or the
# lease changed hands since the claim, so a straddled run is not a green.
[private]
worktree-verify CALLER:
    @bash scripts/worktree-verify.sh "{{CALLER}}"


# Drop this agent session's worktree lease when handing the worktree to another
# session (e.g. an orchestrator's implement then fix sessions).
[group('setup')]
[doc("Release this agent session's worktree lease for the next session.")]
worktree-release:
    @bash scripts/worktree-release.sh worktree-release


# Full root gate set; iterate with scoped checks from docs/development.md first.
# Keep independent gates ahead of tests: a flaky test must not hide rustdoc or
# contract failures. CONTRIBUTING.md owns the local/CI gate map (phux-yb1m).
[group('gates')]
[doc('Full root gate set: the deterministic and unit bar.')]
ci: worktree-claim ci-static test && (worktree-verify "ci")
    @. ./scripts/lib/output.sh; phux_ok "root validation passed"


# Everything in `ci` but the test suite, as two lanes that run side by side:
# the Cargo gates in order (they share one target directory), and the repo
# contracts, which never invoke Cargo. `test` runs after both and alone, so the
# timing-sensitive PTY tests never compete with a compile or a browser.
[private]
[parallel]
ci-static: ci-cargo-gates ci-repo-gates

[private]
ci-cargo-gates: fmt-check lint doc deny build-features-check build-features-compile skill-contract binding-uniffi-test

[private]
ci-repo-gates: docs-check workflow-check font-check e2e-lane-check zig-pin-check ghostty-pin-check toolchain-check install-surface-check cache-portable-check agent-integrations-check


# Full root PR bar, including timing-sensitive e2e and agent example smoke.
# Browser and desktop clients have separate gates; see docs/development.md.
[group('gates')]
[doc('Full root PR bar: ci plus the real-server e2e and agent smoke lanes.')]
ci-full: ci e2e agents-fleet-smoke
    @. ./scripts/lib/output.sh; phux_ok "full root validation passed"
