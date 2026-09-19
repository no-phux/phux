# phux developer commands.
# Mise or Nix gets the tools; this file is how you run the repo after that.
# `just` (no args) lists the public API. CI-internal recipes are [private]
# and still invokable by name (`just fmt-check`, `just ci`, workflows).

default:
    @just --list

# Check prerequisites for only this work area; also works without just via bash.
[group('setup')]
doctor SCOPE="native":
    bash scripts/doctor.sh "{{SCOPE}}"

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

# Native contributor smoke: real compiler/linker/VT build and codec tests, no Nix.
[group('setup')]
native-smoke:
    bash scripts/native-smoke.sh

# Setup helper behavior, including missing tools and rejected compiler downloads.
[group('setup')]
[private]
setup-check:
    bash scripts/test-dev-setup.sh

# Test-lane only: bound auto-spawned daemons (ADR-0063 keeps production
# unattended servers up). Lanes that `env_clear()` re-arm from
# `AutoSpawnedServer::IDLE_BACKSTOP` instead. Do not export this justfile-wide.
AUTO_SPAWN_BACKSTOP := "PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE=600"

# Scaffold a commented starter config into a worktree-local XDG dir
# (./.phux-xdg) so you can test config changes without touching your real
# ~/.config/phux. Re-run freely: `phux config init` refuses to clobber.
# Inspect the result with: XDG_CONFIG_HOME="$PWD/.phux-xdg" phux config show
[group('setup')]
[doc('Scaffold a starter config into a worktree-local XDG dir (./.phux-xdg).')]
scaffold-config:
    XDG_CONFIG_HOME="{{justfile_directory()}}/.phux-xdg" cargo run -q -p phux -- config init

# Quick type-check across the workspace.
[group('build')]
check:
    cargo check --workspace --all-targets

# Build the developer executables (debug).
[group('build')]
build:
    cargo build --locked -p phux -p phux-mcp

# Build every workspace target, including test harnesses, examples and benches.
[group('build')]
build-all:
    cargo build --workspace --all-targets

# Build without browser HTTP/3 support; UDS, WebSocket and raw QUIC remain.
[group('build')]
build-lean:
    cargo build --locked -p phux -p phux-mcp --no-default-features

# Release executables with full LTO.
[group('build')]
build-release:
    cargo build --locked -p phux -p phux-mcp --release

# Build the stable C ABI and the native macOS Cockpit from this checkout.
[group('cockpit')]
[working-directory('clients/cockpit')]
cockpit-build: cockpit-artifacts
    bash ./scripts/build-shipping-app.sh -Dphux-client-ffi-profile=ffi-dev --summary all

# Run the shipping TypeScript graph, native engine regressions, and repository
# and release contract checks.
[group('cockpit')]
[doc('Cockpit TypeScript graph, native engine regressions, and contract checks.')]
[working-directory('clients/cockpit')]
cockpit-test: cockpit-ffi cockpit-build-contracts
    ./scripts/check-release-version.sh
    ./scripts/check-sdk-pin.sh
    ./scripts/lib/zon_test.sh
    ./scripts/lib/measure_test.sh
    ./scripts/zig-build.sh test -Dplatform=null -Dphux-enabled=true -Dphux-client-ffi-profile=ffi-dev --summary all

# Optional full run of the default app graph (DisabledPhuxProvider). CI
# typechecks that graph inside `just cockpit-test` instead of re-running tests.
[group('cockpit')]
[doc('Cockpit tests with the app graph built without the Phux provider.')]
[working-directory('clients/cockpit')]
cockpit-test-no-phux: cockpit-ffi
    ./scripts/zig-build.sh test -Dplatform=null -Dphux-client-ffi-profile=ffi-dev --summary all

# Node TypeScript tests. The loader strips @native-sdk/core's published .ts
# (plain `node --test` on Node 24 will not). Needs clients/cockpit/node_modules.
[group('cockpit')]
[doc('Cockpit Node TypeScript tests via navigation-loader.')]
[working-directory('clients/cockpit')]
cockpit-node-test:
    node --import ./src/tests/navigation-loader.mjs --test ./src/tests/*.test.mjs

# Split out of cockpit-test only because it is the one step that runs from the
# repository root; the rest share [working-directory('clients/cockpit')].
[group('cockpit')]
[private]
cockpit-build-contracts:
    python3 clients/cockpit/scripts/check-build-contracts.py

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

# Build only Cockpit's static archive, with fast unwind-safe development codegen.
[group('cockpit')]
cockpit-ffi:
    cargo rustc --locked --profile ffi-dev -p phux-client-ffi --lib --crate-type staticlib

# Production static FFI archive; release codegen with the required panic boundary.
[group('cockpit')]
cockpit-ffi-release:
    cargo rustc --locked --profile ffi-release -p phux-client-ffi --lib --crate-type staticlib

# Run the isolated developer app with the Phux-backed production graph.
[group('cockpit')]
[working-directory('clients/cockpit')]
cockpit-dev: cockpit-artifacts
    ./scripts/dev-run.sh --phux --ffi-profile ffi-dev

# The FFI + header artifacts both app recipes need, built from the repository
# root before either drops into clients/cockpit.
[group('cockpit')]
[private]
cockpit-artifacts:
    bash clients/cockpit/scripts/build-phux-artifacts.sh ffi-dev

# Build the current checkout and atomically install its developer binaries.
# The binaries live in Cargo's bin dir, matching normal source installs. Keep
# that directory ahead of Homebrew in PATH so there is one developer binary.
[group('build')]
[doc('Build and atomically install the developer binaries into Cargo bin dir.')]
install-dev:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -p phux -p phux-mcp
    install_dir="${CARGO_HOME:-$HOME/.cargo}/bin"
    mkdir -p "$install_dir"
    install -m 755 target/debug/phux "$install_dir/.phux.new"
    install -m 755 target/debug/phux-mcp "$install_dir/.phux-mcp.new"
    mv -f "$install_dir/.phux.new" "$install_dir/phux"
    mv -f "$install_dir/.phux-mcp.new" "$install_dir/phux-mcp"
    echo "installed development binaries to $install_dir"
    echo "phux -> $install_dir/phux"

# Install the rebuilt developer binaries, then hot-swap a server that was
# already started from the source-install path, preserving sessions (ADR-0032).
# A server originally started by Homebrew needs a one-time restart first.
[group('build')]
[doc('Install rebuilt binaries, then hot-swap the running server, keeping sessions.')]
rebuild:
    just install-dev
    "${CARGO_HOME:-$HOME/.cargo}/bin/phux" upgrade

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

# Workspace unit pool via nextest. Same --workspace feature union as e2e/stress
# (CONTRIBUTING.md). Optional profiler surfaces compile under lint/doc instead.
[group('test')]
[doc('Run the workspace unit test pool via nextest.')]
test:
    #!/usr/bin/env bash
    set -euo pipefail
    # Build selection stays --workspace (see the e2e note). CI PRs may set
    # PHUX_NEXTEST_FILTERSET to an rdeps() expression (phux-14r7); unset/empty
    # keeps the full pool, matching pushes to main.
    extra=()
    if [[ -n "${PHUX_NEXTEST_FILTERSET:-}" ]]; then
      extra+=(-E "${PHUX_NEXTEST_FILTERSET}")
    fi
    {{AUTO_SPAWN_BACKSTOP}} cargo nextest run --workspace "${extra[@]}"

# Fast e2e + perf gates (every PR). #[ignore]d out of `just test`; serial +
# retries because these spin a real server+PTY. Keep --workspace so the
# feature union matches `test` (a `-p` selection recompiles tokio). Filterset
# names binaries; `just e2e-lane-check` fails if an ignored *_e2e.rs is in
# no lane. Flag-stability story: CONTRIBUTING.md.
[group('test')]
[doc('Fast e2e + perf gates that spin a real server; every PR.')]
e2e:
    # MCP's discovery integration test makes Cargo build its normal executable
    # in this same graph; first_five_minutes_e2e can copy both payload binaries.
    {{AUTO_SPAWN_BACKSTOP}} cargo nextest run --workspace --run-ignored ignored-only \
      --test-threads=1 --retries=2 \
      -E 'binary_id(phux::automation_e2e) + binary_id(phux::terminal_e2e) + binary_id(phux::recording_e2e) + binary_id(phux::lifecycle_e2e) + binary_id(phux::first_five_minutes_e2e)'
    {{AUTO_SPAWN_BACKSTOP}} cargo nextest run --workspace --run-ignored ignored-only \
      --test-threads=1 --retries=2 \
      -E 'binary_id(phux-server::perf_latency) + binary_id(phux-server::perf_colored_output)'

# Heavy stress storms — off the PR path (post-merge + nightly stress.yml).
# Starvation-sensitive on 2-core runners; run locally. Also hosts
# perf_bursty_output (~110s, allocation count, not wall time).
[group('test')]
[doc('Heavy stress storms; off the PR path (post-merge + nightly).')]
stress:
    cargo nextest run --workspace --run-ignored ignored-only \
      --test-threads=1 --retries=2 \
      -E 'binary_id(phux-server::stress_resize_storm) + binary_id(phux-server::stress_resize_extremes) + binary_id(phux-server::stress_attach_churn) + binary_id(phux-server::stress_lifecycle_churn) + binary_id(phux-server::stress_output_extremes) + binary_id(phux-server::stress_spawn_kill) + binary_id(phux-server::perf_bursty_output)'

# Spins a real `phux` server + session, drives a scripted scenario (heavy
# colored output, a 2nd client attach, a resize storm, an input line) and
# writes screen snapshots + a summary to /tmp/phux-repro-<ts>/ for
# inspection. See crates/phux-server/examples/e2e-repro.rs.
#
# One-command real-server repro of a lag/crash edge case.
[group('test')]
e2e-repro:
    cargo run -p phux-server --example e2e-repro

# Capture a REAL traced client session for the debugging flywheel. Attaches
# with JSON tracing to a timestamped log, then prints the path to hand off
# for analysis. Reproduce the lag/crash during the session (and a crash's
# backtrace lands in the same log), then detach. An auto-spawned server
# inherits the same tracing env, so the log holds both sides (filter by the
# `target` field: phux_client::* vs phux_server::*).
#   just trace-attach                 # session "default"
#   just trace-attach work            # a named session
#   just trace-attach work phux=trace # crank the level
[group('perf')]
[doc('Capture a real traced client session to a timestamped JSON log.')]
trace-attach session="default" level="phux=debug":
    #!/usr/bin/env bash
    set -euo pipefail
    log="/tmp/phux-trace-$(date +%s).json"
    echo "[trace] -> $log  (PHUX_LOG_FORMAT=json, RUST_LOG={{level}}); reproduce the issue, then detach"
    PHUX_LOG="$log" PHUX_LOG_FORMAT=json RUST_LOG="{{level}}" cargo run -q -p phux -- attach {{session}} || true
    echo "[trace] session ended -> hand off this file: $log"
    echo "[trace] quick peek at the slowest renders:"
    jq -rc 'select(.fields.message=="close" and (.span.name|test("render|handle_server_frame|synthesize|tick_emit"))) | [.fields["time.busy"], .span.name, (.span.changed_row_count//.span.out_bytes//"")] | @tsv' "$log" 2>/dev/null | sort -h | tail -15 || true

# Live performance telemetry of the running server (ADR-0096). One row per
# hot-path metric, one interval per second; Ctrl-C to stop. Same as
# `phux perf --watch 1` on the installed binary, built from this tree.
[group('perf')]
[doc('Live performance telemetry of the running server (ADR-0096).')]
perf interval="1":
    cargo run -q -p phux -- perf --watch {{interval}}

# Reproducible echo latency: an isolated release server, a probe pane and a
# flooding sibling pane (`full` repaints the whole screen at 30 fps,
# `spinner` one line at 10 Hz, `none` for a quiet baseline), keystroke echo
# measured at the pty byte level. Raw JSON and CPU samples land under the
# printed artifacts directory. Never touches a running server.
#   just perf-echo                  # 188x48, full flood
#   just perf-echo none 120 40      # quiet baseline at 120x40
[group('perf')]
[doc('Reproducible echo-latency benchmark against an isolated release server.')]
perf-echo flood="full" cols="188" rows="48" iters="60":
    cargo build --release -p phux
    bash scripts/bench/tui-load.sh target/release/phux "{{flood}}-{{cols}}x{{rows}}" {{flood}} {{cols}} {{rows}} {{iters}}

# Smoke-test the examples/agents/ scripts against a throwaway server, so
# they cannot rot silently against CLI changes (phux-wiv). Builds `phux`
# once, pins SHELL=/bin/sh for a banner-free seed pane (no p10k/direnv
# noise in snapshots), then runs every example and fails on any non-zero
# exit. Like `e2e` it spawns real PTY-backed servers, so it stays OUT of
# the parallel `ci` pool and runs on demand or as its own CI step.
[group('test')]
[doc('Smoke-test the examples/agents/ scripts against a throwaway server.')]
examples-smoke:
    bash scripts/examples-smoke.sh

# Hermetic argv/control-flow gate for the placed-fleet worked example. Uses a
# fake phux binary, so it needs neither a server nor installed agent CLIs.
[group('test')]
[doc('Hermetic argv and control-flow gate for the placed-fleet example.')]
agents-fleet-smoke:
    bash examples/agents/tests/placed-fleet-smoke.sh

# Real isolated server dogfood for placement/layout/watch/ask with shell panes;
# no external agent binary is needed. Set PHUX_DOGFOOD_REAL_AGENTS=1 to also
# spawn installed claude/codex binaries on the private server.
[group('test')]
[doc('Real isolated-server dogfood for placement, layout, watch, and ask.')]
agents-fleet-live:
    cargo build -p phux
    PHUX="{{justfile_directory()}}/target/debug/phux" \
      bash examples/agents/tests/placed-fleet-live.sh

# Run the checked-in plugin package through the same discover/validate/run
# sequence documented in examples/plugins/agent-tools/README.md.
[group('test')]
[doc('Run the checked-in plugin package through discover, validate, and run.')]
plugin-demo:
    #!/usr/bin/env bash
    set -euo pipefail
    export XDG_CONFIG_HOME="{{justfile_directory()}}/examples/plugins/agent-tools/config"
    demo() { cargo run -q -p phux -- "$@"; }
    demo config plugins
    demo config plugins --json
    demo config run com.phux.demo.agent-tools inspect
    demo config run com.phux.demo.agent-tools inspect --json
    demo config run com.phux.demo.agent-tools list-integrations
    demo config run com.phux.demo.agent-tools validate-integrations
    demo config run com.phux.demo.agent-tools status-integrations
    demo config run com.phux.demo.agent-tools smoke-integrations
    demo config run com.phux.demo.agent-tools detect-agents

# List and verify the herdr parity QA gate without running heavy surfaces.
[group('test')]
parity-check-list:
    bash scripts/parity-gate.sh --check-list

# Run the herdr parity QA gate. With no args, runs every parity scenario;
# pass scenario names to run a subset, e.g. `just parity-gate plugin-demo`.
[group('test')]
[doc('Run the herdr parity QA gate: all scenarios, or the named subset.')]
parity-gate *SCENARIOS:
    bash scripts/parity-gate.sh --run {{SCENARIOS}}

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

# Stable-cargo test for environments without nextest.
[group('test')]
test-cargo:
    {{AUTO_SPAWN_BACKSTOP}} cargo test --workspace

# Dependency hygiene: licenses, advisories, bans.
[group('gates')]
deny:
    cargo deny check

# Default, lean and headless production dependency boundaries (compile-free).
[group('gates')]
[private]
build-features-check:
    python3 scripts/check-build-features.py

# Compile opt-out consumers separately: workspace feature unification masks them.
[group('build')]
[private]
build-features-compile:
    cargo check --locked -p phux-client -p phux-mcp --all-targets --no-default-features --features phux-client/testkit
    cargo check --locked -p phux --no-default-features --bin phux
    cargo check --locked -p phux-tui --all-targets --no-default-features

# Build rustdoc with warnings denied — mirrors the CI `doc` gate.
[group('gates')]
doc:
    RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features

# Watch loop — re-check + test on every save.
[group('build')]
watch:
    cargo watch -x check -x 'nextest run --workspace'

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

# Rust's native manifest, verified Zig manifest, Mise, the Nix flake, CI,
# standalone WASM workspaces, and pinned container builders agree on their
# toolchains. Static: reads files, so it runs on a CI checkout with no Nix, no
# Mise and no compilers, which is why it can be a `ci` gate.
[group('setup')]
[doc('Toolchain pins agree across manifests, Mise, Nix, CI, and containers.')]
toolchain-check:
    bash scripts/check-toolchain-sync.sh

# Runtime half of toolchain-check: both environments, actual binaries.
# Not in `ci` (no Mise on a CI checkout). Skips if either env is missing.
[group('setup')]
toolchain-parity:
    bash scripts/check-toolchain-parity.sh

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

# Cyclomatic complexity of production code over a CCN ceiling — advisory.
# clippy.toml is the CI gate; this is lizard via uvx. Not in `ci`.
[group('perf')]
complexity CCN="15":
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v uvx >/dev/null 2>&1; then
      echo "complexity: uvx not found; install uv to run this report" >&2
      exit 1
    fi
    uvx --from lizard==1.24.0 lizard -l rust crates --CCN {{CCN}} -w \
      | grep -v -e '/tests/' -e '/benches/' -e '/examples/' \
      || echo "no production function exceeds CCN {{CCN}}"

# Every non-closed bead carries exactly one of rc-1.0 / post-1.0 — advisory, local-only.
[group('release')]
milestone-check:
    node scripts/check-milestone-labels.mjs

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

# Are any releases stuck? Reports drafts that never published, published
# releases with no assets, merged release PRs release-please never tagged, and
# manifest versions with no release. Needs an authenticated `gh`, so it is NOT
# in `ci` — it reads live GitHub state, not the working tree. release-drift.yml
# runs it daily; this is the same check, on demand.
[group('release')]
[doc('Report stuck releases: drafts, assetless publishes, untagged release PRs.')]
release-drift grace="120":
    GRACE_MINUTES={{ grace }} node scripts/check-release-drift.mjs

# Full root gate set; iterate with scoped checks from docs/SETUP.md first.
# Keep independent gates ahead of tests: a flaky test must not hide rustdoc or
# contract failures. CONTRIBUTING.md owns the local/CI gate map (phux-yb1m).
[group('gates')]
[doc('Full root gate set: the deterministic and unit bar.')]
ci: fmt-check lint doc deny build-features-check build-features-compile docs-check workflow-check formula-check font-check e2e-lane-check zig-pin-check toolchain-check install-surface-check skill-contract agent-integrations-check test
    @echo "ok"

# Full root PR bar, including timing-sensitive e2e and agent example smoke.
# Browser and Cockpit clients have separate gates; see docs/SETUP.md.
[group('gates')]
[doc('Full root PR bar: ci plus the real-server e2e and agent smoke lanes.')]
ci-full: ci e2e agents-fleet-smoke
    @echo "ok (full)"

# Print the toolchain we are pinned to.
[group('setup')]
toolchain:
    @rustc --version
    @cargo --version

# Package the host-target release binaries into a tarball via
# scripts/pack-release.sh (phux-<tag>-<target>.tar.gz) under dist/. Used
# to seed the first Homebrew release locally; CI does this per-target on a
# `v*` tag. Pass the tag, e.g. `just dist v0.0.1`.
[group('release')]
[doc('Package host-target release binaries into dist/, matching release.yml naming.')]
dist TAG:
    bash scripts/dist.sh {{TAG}}

# Local release preflight before pressing the GitHub Actions release button.
# Runs version/tag checks, install-surface drift checks, formula generation,
# and a phux-protocol crates.io package dry-run.
[group('release')]
[doc('Release preflight: version/tag, install surface, formula, crate dry-run.')]
release-preflight TAG:
    bash scripts/release-preflight.sh {{TAG}}

# Same release preflight, but skip the crates.io dry-run when offline or when
# this is a binary/Homebrew-only release and cargo registry access is flaky.
[group('release')]
[doc('Release preflight without the crates.io dry-run, for offline or binary-only.')]
release-preflight-fast TAG:
    bash scripts/release-preflight.sh {{TAG}} --skip-crate-dry-run

# Check that a release tag matches the resolved Cargo package versions.
[group('release')]
release-check TAG:
    bash scripts/check-release-version.sh {{TAG}}

# Dry-run the crates.io publish of phux-protocol (package + verify, no
# upload). The only publishable crate. Mirrors the publish-crate workflow.
[group('release')]
[doc('Dry-run the phux-protocol crates.io publish: package and verify, no upload.')]
publish-protocol-dry:
    cargo publish --locked --dry-run -p phux-protocol

# Publish phux-protocol to crates.io. IRREVERSIBLE. Requires `cargo login`
# (or CARGO_REGISTRY_TOKEN). Run `just publish-protocol-dry` first.
[group('release')]
[doc('Publish phux-protocol to crates.io. IRREVERSIBLE.')]
[confirm('Publish phux-protocol to crates.io? This cannot be undone. [y/N]')]
publish-protocol:
    cargo publish --locked -p phux-protocol

# CPU-profile the phux binary with samply (not a workspace dep).
# `just profile` records `phux server`; pass args for another subcommand.
[group('perf')]
profile *ARGS:
    @if ! command -v samply >/dev/null 2>&1; then \
        echo "error: samply not found on PATH." >&2; \
        echo "  install it with:  cargo install samply" >&2; \
        echo "  (samply is intentionally not a workspace dep; it is a host tool)" >&2; \
        exit 127; \
    fi
    cargo build --profile profiling --bin phux
    @echo ""
    @echo "Recording profile -> target/samply-profile.json"
    @echo "  Stop the profiled process (Ctrl-C) to finalize the recording."
    @echo ""
    samply record --output target/samply-profile.json -- target/profiling/phux {{ if ARGS == "" { "server" } else { ARGS } }}
    @echo ""
    @echo "Profile written to target/samply-profile.json"
    @echo "  View it with:  samply load target/samply-profile.json"
    @echo "  (opens https://profiler.firefox.com in your browser)"

# Build observability (Rust side only; the cold Zig blob is invisible here).
[group('perf')]
timings *ARGS:
    cargo build --workspace --all-targets --timings {{ARGS}}
    @echo "report -> target/cargo-timings/cargo-timing.html"

[group('perf')]
llvm-lines PKG='phux-protocol' *ARGS:
    cargo llvm-lines -p {{PKG}} {{ARGS}}

[group('perf')]
bloat *ARGS:
    cargo bloat --release --bin phux {{ if ARGS == "" { "--crates" } else { ARGS } }}

# Dependency-graph stats without compiling: locked-package count, duplicate
# versions (each compiles separately in cold CI), proc-macro and
# build-script crate counts. Prints markdown to stdout.
[group('perf')]
[doc('Dependency-graph stats without compiling: counts, duplicates, proc-macros.')]
dep-stats:
    bash scripts/ci/dep-stats.sh
