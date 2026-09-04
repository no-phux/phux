# phux developer commands.
# Run `just` (no args) to list them.

default:
    @just --list

# Bound every daemon the test lanes can AUTO-SPAWN (phux-whhd, phux-nbam).
#
# A test that spawns `phux server` itself passes `--exit-after-idle 600` as
# its survives-a-SIGKILLed-runner backstop, on top of a `Drop` guard. The
# auto-spawn path can do neither on its own: `maybe_auto_spawn_server`
# deliberately orphans a daemonised child, and the last-pane self-exit is
# armed only once a client has attached, so a daemon nobody attached to never
# exits by itself. Kill a test process before its `Drop` guard runs — a
# cancelled agent run, a reaped job — and what leaks is immortal.
#
# Not hypothetical: three such daemons were found alive on a developer box
# three days after the runs that started them, each still holding a PTY on a
# `tempfile` socket whose directory had long since been removed.
#
# `PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE` is the opt-in seam phux-nbam added for
# exactly this, and it stays off in production on purpose: ADR-0063 pins that
# an unattended server stays up, so making auto-spawn finite by default would
# change the multiplexer contract. Applying it here — on the test lanes only,
# not as a `justfile`-wide `export` that would also reach `install-dev`,
# `rebuild` and the `cargo run` recipes — bounds the harness without touching
# what a developer's own server does.
#
# 600s is far longer than any gap between a test's client connections, so the
# backstop can only fire once the harness is already gone. `idle_exit_e2e`
# drives both the set and the unset case and `env_remove`s it for the latter,
# so inheriting it here does not weaken that coverage.
#
# CAVEAT (phux-8y3o): this export cannot survive a lane that `env_clear()`s
# the processes it spawns — the variable is wiped before it reaches the
# auto-spawning parent. The rule for such a lane is not carried here any
# more: it belongs to the type that defines the hazard, as
# `AutoSpawnedServer::IDLE_BACKSTOP` in `crates/phux/tests/common/mod.rs`,
# which names the server's own constant. A hermetic harness re-arms from
# there rather than from a literal of its own.
AUTO_SPAWN_BACKSTOP := "PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE=600"

# Scaffold a commented starter config into a worktree-local XDG dir
# (./.phux-xdg) so you can test config changes without touching your real
# ~/.config/phux. Re-run freely: `phux config init` refuses to clobber.
# Inspect the result with: XDG_CONFIG_HOME="$PWD/.phux-xdg" phux config show
scaffold-config:
    XDG_CONFIG_HOME="{{justfile_directory()}}/.phux-xdg" cargo run -q -p phux -- config init

# Quick type-check across the workspace.
check:
    cargo check --workspace --all-targets

# Build all crates (debug).
build:
    cargo build --workspace --all-targets

# Release build with full LTO.
build-release:
    cargo build --locked --workspace --release

# Build the stable C ABI and the native macOS Cockpit from this checkout.
cockpit-build: cockpit-ffi
    cd clients/cockpit && ./scripts/zig-build.sh -Dphux-enabled=true --summary all

# Run both Cockpit graphs plus the repository and release contract checks.
cockpit-test: cockpit-ffi
    cd clients/cockpit && ./scripts/check-release-version.sh
    cd clients/cockpit && ./scripts/check-sdk-pin.sh
    cd clients/cockpit && ./scripts/lib/zon_test.sh
    cd clients/cockpit && ./scripts/lib/measure_test.sh
    cd clients/cockpit && ./scripts/zig-build.sh test -Dplatform=null -Dphux-enabled=true --summary all
    cd clients/cockpit && ./scripts/zig-build.sh test -Dtypescript-spike=true -Dplatform=null --summary all

# Build Cockpit's stable C ABI dependency from the enclosing Phux workspace.
cockpit-ffi:
    cargo build --locked --profile ffi-release -p phux-client-ffi

# Run the isolated developer app with the Phux-backed production graph.
cockpit-dev: cockpit-ffi
    cd clients/cockpit && ./scripts/dev-run.sh --phux

# Build the current checkout and atomically install its developer binaries.
# The binaries live in Cargo's bin dir, matching normal source installs. Keep
# that directory ahead of Homebrew in PATH so there is one developer binary.
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
rebuild:
    just install-dev
    "${CARGO_HOME:-$HOME/.cargo}/bin/phux" upgrade

# Format every Rust file in place.
fmt:
    cargo fmt --all

# CI-style format check — fails if anything is dirty.
fmt-check:
    cargo fmt --all -- --check

# Clippy with warnings denied. The bar.
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# The iteration loop: everything in `ci` that is fast and catches most of what
# `ci` rejects, in the order `ci` would hit it, with the same flags.
#
# The flags matter more than the list. `lint` resolves --all-features and
# `test` resolves default features (see `test`'s comment), so those are two
# distinct build graphs; running either with the *wrong* flags produces
# artifacts `ci` cannot reuse and rebuilds the world. Iterating on `just ci`
# itself is the expensive mistake this recipe exists to prevent: it fail-fasts,
# so one clippy nit costs a full re-run of every leg before it.
#
# Run this until clean, then `just ci` ONCE as the final gate.
#
# `e2e` is included deliberately, even though it is the slow leg. CI's `test`
# job runs unit AND e2e; `just ci` runs only the unit pool, so a green `ci`
# locally is NOT the same bar as a green PR. Two failures reached CI that way
# before this line existed. If you want the fast inner loop, run `fmt`,
# `lint`, and `test` directly — but do not call it clean without this.
#
# NOTE: no backticks in the echo below — `just` runs backticks as a shell
# command, so a friendly "run `just ci`" hint would actually RUN `just ci`
# on every precommit, which is precisely the waste this recipe avoids.
precommit: fmt lint docs-gen test e2e
    @echo "precommit clean - now run 'just ci' once to confirm the remaining gates"

# Run tests via nextest (parallel, sane output).
#
# Deliberately NOT --all-features: ci.yml's unit lane runs default features,
# and --all-features turns on phux/dhat-heap, which swaps the global allocator
# for dhat's in every spawned `phux` binary — all ~285 CLI tests then run
# under a profiling allocator CI never uses (measured ~30-50s slower) and
# each clean exit writes a dhat-heap.json. It also resolves a different
# feature union than the `e2e` recipe, forcing a second full build locally.
# Feature-gated code still compiles under `lint` and `doc` (--all-features).
# Verified: the test list is identical with and without --all-features.
test:
    {{AUTO_SPAWN_BACKSTOP}} cargo nextest run --workspace

# Fast e2e lane — gates every PR (the `e2e` step in ci.yml). Covers the
# headless agent-surface contract (`run_wait_e2e`), the ADR-0040 agent
# identity record loop (`agent_record_e2e`), real attached-client spatial
# edits (`spatial_e2e`), plus the wall-clock perf
# gates (`perf_latency`, `perf_colored_output`). These spin a real server +
# PTY, so they are `#[ignore]`d out of the default `just test` pool and run
# serially with `--retries=2`: serial removes the CPU contention that makes
# a fresh attach handshake / snapshot render miss `WIRE_RECV_TIMEOUT`, and
# the retries absorb residual environment-driven flakes (mirroring the
# reconnect override in .config/nextest.toml). Finishes in minutes — fast
# enough to block a PR on.
#
# The BUILD selection is `--workspace` on purpose, even though only four test
# binaries actually run. It must resolve the SAME feature union as the unit
# lane's `cargo nextest run --workspace`, so this lane reuses that build
# instead of producing a second one.
#
# Narrowing the BUILD with `-p`/`--test` is what the old form did, and it cost
# ~87s per CI run. Under the v2/v3 feature resolver a package's dev-dependency
# features only join the unified feature set for packages whose test targets
# are being built, so `-p phux` drops phux-server's dev-deps (tokio/test-util,
# wtransport/dangerous-configuration) and `-p phux-server` drops phux-client's
# tokio/io-std. Each selection therefore re-keys `tokio` into a DIFFERENT unit,
# and every crate downstream of tokio (phux-core, phux-server, phux-client,
# phux, quinn, tokio-util, tokio-rustls, ...) recompiles from scratch.
#
# Test selection is a nextest filterset instead, which is applied AFTER the
# build and so costs nothing. Verified: same 18 tests, 0 crates recompiled.
#
# The filterset names binaries, not files, so renaming/moving one of these test
# files does NOT silently drop it from the PR gate: nextest rejects a
# `binary_id(...)` that matches no binary ("operator didn't match any binary
# IDs") and exits 94. A rename fails this lane loudly, exactly as `--test
# <name>` used to.
#
# What a rename does NOT catch is a brand-new `*_e2e.rs` that nobody adds
# here: its `#[ignore]`d tests would then run in no lane at all and sit green
# forever. `just e2e-lane-check` (scripts/check-e2e-lanes.sh, a `just ci`
# gate) closes that: every `crates/*/tests/*_e2e.rs` carrying an `#[ignore]`
# must be named by this recipe or by `stress`. It found three binaries in
# exactly that state — plugin_agent_bench_e2e, upgrade_e2e (whose own module
# doc claimed "run via `just e2e`"), and workspace_archive_e2e — which is why
# the filterset below now names them. They add ~2.6s to the lane.
#
# Corollary: if ci.yml's unit step ever gains `--all-features`, this recipe
# must gain it too — otherwise the double-compile comes straight back. (Do not
# actually do that; `--all-features` turns on `phux/dhat-heap`, which installs
# dhat as the global allocator and would make the perf gates below measure
# dhat rather than phux.)

# Fast e2e lane (every #[ignore]d phux e2e binary + the perf gates) — gates every PR.
e2e:
    # first_five_minutes_e2e copies both release payload binaries into a fresh prefix.
    cargo build -p phux-mcp
    {{AUTO_SPAWN_BACKSTOP}} cargo nextest run --workspace --run-ignored all \
      --test-threads=1 --retries=2 \
      -E 'binary_id(phux::run_wait_e2e) + binary_id(phux::agent_record_e2e) + binary_id(phux::spatial_e2e) + binary_id(phux::rec_e2e) + binary_id(phux::resize_e2e) + binary_id(phux::play_e2e) + binary_id(phux::idle_exit_e2e) + binary_id(phux::plugin_agent_bench_e2e) + binary_id(phux::upgrade_e2e) + binary_id(phux::workspace_archive_e2e) + binary_id(phux::failure_ux_e2e) + binary_id(phux::first_five_minutes_e2e) + binary_id(phux::fleet_sidebar_e2e) + binary_id(phux::remote_target_e2e)'
    {{AUTO_SPAWN_BACKSTOP}} cargo nextest run --workspace --run-ignored ignored-only \
      --test-threads=1 --retries=2 \
      -E 'binary_id(phux-server::perf_latency) + binary_id(phux-server::perf_colored_output)'

# Heavy stress/flywheel lane — runs OFF the PR critical path (the `stress`
# GitHub workflow: post-merge on `main` + nightly). Resize/output/lifecycle
# storms that hammer a real server + PTY. They are CPU-starvation-sensitive:
# the server is one current-thread runtime, and on a 2-core runner the
# output-flood-vs-resize-reflow feedback loop balloons a sub-second test
# into minutes (e.g. both_axes_shrink_storm_under_output: ~0.3s on a
# multi-core box, ~13 min on a 2-core runner). That cost is pure CPU
# starvation, not a code defect — so these run where they don't block a PR,
# never as a `just ci` gate. Run locally any time (one binary at a time,
# they pass reliably).

# The lane also hosts `perf_bursty_output`: NOT starvation-sensitive (it
# gates an allocation count, not wall time), just ~110s of CPU-bound
# full-churn synthesis that was the single longest test in the PR unit
# pool. Off the PR path it costs nothing; a regression still trips
# post-merge/nightly.

# Heavy stress storms — off the PR path (post-merge + nightly stress.yml).
stress:
    cargo nextest run -p phux-server --run-ignored ignored-only \
      --test-threads=1 --retries=2 \
      --test stress_resize_storm --test stress_resize_extremes \
      --test stress_attach_churn --test stress_lifecycle_churn \
      --test stress_output_extremes --test stress_spawn_kill \
      --test perf_bursty_output

# Spins a real `phux` server + session, drives a scripted scenario (heavy
# colored output, a 2nd client attach, a resize storm, an input line) and
# writes screen snapshots + a summary to /tmp/phux-repro-<ts>/ for
# inspection. See crates/phux-server/examples/e2e-repro.rs.
#
# One-command real-server repro of a lag/crash edge case.
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
perf interval="1":
    cargo run -q -p phux -- perf --watch {{interval}}

# Reproducible echo latency: an isolated release server, a probe pane and a
# flooding sibling pane (`full` repaints the whole screen at 30 fps,
# `spinner` one line at 10 Hz, `none` for a quiet baseline), keystroke echo
# measured at the pty byte level. Raw JSON and CPU samples land under the
# printed artifacts directory. Never touches a running server.
#   just perf-echo                  # 188x48, full flood
#   just perf-echo none 120 40      # quiet baseline at 120x40
perf-echo flood="full" cols="188" rows="48" iters="60":
    cargo build --release -p phux
    bash scripts/bench/tui-load.sh target/release/phux "{{flood}}-{{cols}}x{{rows}}" {{flood}} {{cols}} {{rows}} {{iters}}

# Smoke-test the examples/agents/ scripts against a throwaway server, so
# they cannot rot silently against CLI changes (phux-wiv). Builds `phux`
# once, pins SHELL=/bin/sh for a banner-free seed pane (no p10k/direnv
# noise in snapshots), then runs every example and fails on any non-zero
# exit. Like `e2e` it spawns real PTY-backed servers, so it stays OUT of
# the parallel `ci` pool and runs on demand or as its own CI step.
examples-smoke:
    bash scripts/examples-smoke.sh

# Hermetic argv/control-flow gate for the placed-fleet worked example. Uses a
# fake phux binary, so it needs neither a server nor installed agent CLIs.
agents-fleet-smoke:
    bash examples/agents/tests/placed-fleet-smoke.sh

# Real isolated server dogfood for placement/layout/watch/ask with shell panes;
# no external agent binary is needed. Set PHUX_DOGFOOD_REAL_AGENTS=1 to also
# spawn installed claude/codex binaries on the private server.
agents-fleet-live:
    cargo build -p phux
    PHUX="{{justfile_directory()}}/target/debug/phux" \
      bash examples/agents/tests/placed-fleet-live.sh

# Run the checked-in plugin package through the same discover/validate/run
# sequence documented in examples/plugins/agent-tools/README.md.
plugin-demo:
    XDG_CONFIG_HOME="{{justfile_directory()}}/examples/plugins/agent-tools/config" cargo run -q -p phux -- config plugins
    XDG_CONFIG_HOME="{{justfile_directory()}}/examples/plugins/agent-tools/config" cargo run -q -p phux -- config plugins --json
    XDG_CONFIG_HOME="{{justfile_directory()}}/examples/plugins/agent-tools/config" cargo run -q -p phux -- config run com.phux.demo.agent-tools inspect
    XDG_CONFIG_HOME="{{justfile_directory()}}/examples/plugins/agent-tools/config" cargo run -q -p phux -- config run com.phux.demo.agent-tools inspect --json
    XDG_CONFIG_HOME="{{justfile_directory()}}/examples/plugins/agent-tools/config" cargo run -q -p phux -- config run com.phux.demo.agent-tools list-integrations
    XDG_CONFIG_HOME="{{justfile_directory()}}/examples/plugins/agent-tools/config" cargo run -q -p phux -- config run com.phux.demo.agent-tools validate-integrations
    XDG_CONFIG_HOME="{{justfile_directory()}}/examples/plugins/agent-tools/config" cargo run -q -p phux -- config run com.phux.demo.agent-tools status-integrations
    XDG_CONFIG_HOME="{{justfile_directory()}}/examples/plugins/agent-tools/config" cargo run -q -p phux -- config run com.phux.demo.agent-tools smoke-integrations
    XDG_CONFIG_HOME="{{justfile_directory()}}/examples/plugins/agent-tools/config" cargo run -q -p phux -- config run com.phux.demo.agent-tools detect-agents

# List and verify the herdr parity QA gate without running heavy surfaces.
parity-check-list:
    bash scripts/parity-gate.sh --check-list

# Run the herdr parity QA gate. With no args, runs every parity scenario;
# pass scenario names to run a subset, e.g. `just parity-gate plugin-demo`.
parity-gate *SCENARIOS:
    bash scripts/parity-gate.sh --run {{SCENARIOS}}

# Lint shell scripts with shellcheck (the harness, the boundary/docs
# guards, and the examples). Provided by the dev shell. Gates at
# `warning` severity: the examples carry deliberate `info`-level nits
# (sourced libs shellcheck can't follow, single-quoted heredoc-ish
# program strings) that are correct as written. On-demand, not in `ci`.
shellcheck:
    shellcheck --severity=warning scripts/*.sh scripts/ci/*.sh \
      examples/agents/*.sh \
      examples/agents/orchestrate-placed-fleet examples/agents/tests/*.sh \
      examples/plugins/*/scripts/*.sh

# GitHub workflow + composite-action syntax, the fail-closed CI path-routing
# truth table, and the SHA-pin policy for action references.
workflow-check:
    actionlint .github/workflows/*.yml
    bash scripts/ci/check-classify-changes.sh
    bash scripts/check-action-pins.sh
    node scripts/check-release-orchestration.mjs
    node scripts/check-release-drift-policy.mjs

# Stable-cargo test for environments without nextest.
test-cargo:
    {{AUTO_SPAWN_BACKSTOP}} cargo test --workspace --all-features

# Dependency hygiene: licenses, advisories, bans.
deny:
    cargo deny check

# Build rustdoc with warnings denied — mirrors the CI `doc` gate.
doc:
    RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features

# Watch loop — re-check + test on every save.
watch:
    cargo watch -x check -x 'nextest run --workspace'

# Doc system gates: frontmatter, TL;DR, dead links, ADR status, spec version.
# See docs/CONVENTIONS.md.
docs-check:
    bash scripts/check-docs.sh

# Regenerate the generated reference docs (docs/reference/) from the compiled
# binary. Run after any change to the CLI surface: a freshness unit test in
# crates/phux/src/refdocs/ byte-compares the tree against this generator's
# output on every `just test`, so a stale tree fails CI with this recipe as
# the remedy. Idempotent. See docs/CONVENTIONS.md §"Generated reference docs".
docs-gen:
    cargo run -q -p phux --bin phux -- gen-reference-docs

# Homebrew formula generator vs the shapes release.yml's matrix can produce.
formula-check:
    bash scripts/check-formula.sh

# release.yml's pinned Zig tarball digests vs ziglang.org's published index.
# Skips (exit 0) when the index is unreachable, so it is safe offline.
zig-pin-check:
    bash scripts/check-zig-pins.sh

# Install/release documentation contracts (README, INSTALL, RELEASING, the
# installer, the formula generator, release.yml). Also run by
# `just release-preflight`; in `ci` so it cannot rot between releases.
install-surface-check:
    bash scripts/check-install-surface.sh

# Every released agent-facing binary embeds a configless, EPIPE-safe --skill.
skill-contract:
    cargo build -p phux -p phux-mcp
    bash scripts/check-skill-contract.sh

# Drift gate for the one generated Rust source in the tree:
# crates/phux-record/src/font/spleen_8x16.rs vs its vendored .bdf. Compile-free
# (python3 + cmp). See scripts/check-generated-font.sh for why this is a check
# and not a build.rs.

# Generated glyph table vs its .bdf source — catches hand edits and stale regens.
font-check:
    bash scripts/check-generated-font.sh

# Coverage gate for the `e2e` filterset above: every `crates/*/tests/*_e2e.rs`
# carrying an `#[ignore]` must be named by a lane that runs ignored tests,
# otherwise it executes nowhere and stays green on nothing. Compile-free.

# Every #[ignore]d e2e binary is named by some lane — no test rots unrun.
e2e-lane-check:
    bash scripts/check-e2e-lanes.sh

# Release-milestone label coverage in the beads tracker: every non-closed bead
# carries exactly one of `rc-1.0` / `post-1.0`, so "what is left for 1.0" — a
# label query — cannot silently undercount (phux-i7vu, phux-axdt).
#
# DELIBERATELY NOT IN `ci`. This queries the live Dolt store through `bd`; a
# CI checkout has no store (`.beads/embeddeddolt/` is gitignored) and the
# tracked `.beads/issues.jsonl` is a passive, deliberately scrubbed export, so
# the only CI-shaped implementation would be one that reads a snapshot and is
# wrong in both directions. Advisory and local: run it at session close. It
# skips with exit 0 when there is no store, and never prints a verdict about
# labels it could not read. See the header of the script for the full argument.

# Cyclomatic-complexity report for production code, worst function first.
#
# The enforced ceiling is `cognitive-complexity-threshold` in clippy.toml,
# which `just lint` applies on every run and CI therefore gates. This recipe
# is the richer second opinion: clippy measures *cognitive* complexity, which
# discounts a flat `match` arm, while lizard measures classic cyclomatic
# complexity and counts every decision point. A dispatch table that clippy is
# content with still shows up here, which is what you want when deciding
# whether a function has grown a second responsibility.
#
# DELIBERATELY NOT IN `ci`. lizard is fetched on demand with `uvx` rather than
# pinned in the dev shell, so it needs a network on first run and cannot be a
# hard gate without making a CI checkout depend on PyPI. clippy.toml carries
# the gate; this carries the detail.
#
# Reads its threshold from the argument, defaulting to the ceiling the
# codebase was brought to in the complexity pass: no production function
# exceeds 15. Tests, benches and examples are excluded — a long table-driven
# test is not the same defect as a long handler.
#
#   just complexity        # anything over CCN 15
#   just complexity 10     # tighter sweep, for finding the next candidates

# Cyclomatic complexity of production code over a CCN ceiling — advisory, local-only.
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
milestone-check:
    node scripts/check-milestone-labels.mjs

# Everything CI must pass, minus the lanes that need a real server (see
# `ci-full`). This is the inner-loop bar: everything here is deterministic
# and machine-independent, so a green run predicts a green `check` job.
#
# The gate list mirrors ci.yml's `check` job step for step — fmt, clippy,
# rustdoc, deny, docs-check, formula-check, font-check, e2e-lane-check,
# zig-pin-check, install-surface-check, skill-contract — plus the unit test pool from the `test`
# job. Keep it that way:
# `just ci` losing a gate CI still runs is how PR #306 shipped five rustdoc
# failures to a red build after a green local run.
#
# `test` must NOT precede any deterministic, test-independent gate (doc,
# deny, the bash guard rails). `just` fail-fasts on the first non-zero
# recipe, so a gate placed after `test` silently stops running the moment a
# test flakes on that machine -- this repo has load-dependent flakes, and
# that exact ordering bug is how two more branches (phux-j1zj,
# phux-w7z2.59) reached CI with private-intra-doc-links failures despite a
# green local `just ci` (phux-yb1m). Keep every gate whose result does not
# depend on the test suite ahead of `test`.
#
# The ratatui-confinement boundary (ADR-0020) used to be a grep guard
# (`check-ratatui-boundary`); phux-0fv replaced it with a crate split, so
# `cargo build`/`lint` now enforce it structurally — `phux-client-core` has
# no `ratatui` dependency.

# The inner-loop bar — every deterministic gate CI runs. Run before pushing.
agent-integrations-check:
    #!/usr/bin/env bash
    set -euo pipefail
    # Incidental install-time audits are off (npm's advisory endpoints have
    # 503'd mid-lane and installs don't need them; npm stops project-config
    # discovery at the nearest package.json, so env vars — not a root .npmrc
    # — are what reach every spawned npm call). The explicit `npm audit`
    # gates stay on, wrapped in scripts/npm-audit-gate.mjs for outage retry.
    export npm_config_audit=false npm_config_fund=false
    node scripts/check-agent-integration-versions.mjs
    for package in integrations/opencode integrations/pi integrations/claude; do
      npm --prefix "$package" ci
      npm --prefix "$package" run gates
    done

# Are any releases stuck? Reports drafts that never published, published
# releases with no assets, merged release PRs release-please never tagged, and
# manifest versions with no release. Needs an authenticated `gh`, so it is NOT
# in `ci` — it reads live GitHub state, not the working tree. release-drift.yml
# runs it daily; this is the same check, on demand.
release-drift grace="120":
    GRACE_MINUTES={{ grace }} node scripts/check-release-drift.mjs

# The inner-loop bar — every deterministic gate CI runs. Run before pushing.
ci: fmt-check lint doc deny docs-check workflow-check formula-check font-check e2e-lane-check zig-pin-check install-surface-check skill-contract agent-integrations-check test
    @echo "ok"

# The COMPLETE PR bar: `ci` plus the two lanes that spawn real processes.
#
# These are split out of `ci` rather than folded in because they are not
# machine-independent: `e2e` spawns real PTY-backed servers and ends with two
# wall-clock perf ceilings (`perf_latency`, `perf_colored_output`), which a
# laptop under load can miss for reasons that say nothing about the diff.
# Keeping them out of `ci` keeps the inner loop honest — a `ci` failure always
# means a real defect — while this recipe reproduces what a PR is actually
# judged on. Run it before pushing anything that touches the CLI surface, the
# server lifecycle, or the example scripts.
#
# Not covered locally: the sccache/rust-cache/lane-signal steps (runner
# infrastructure) and the deploy-key-gated release lanes. See CONTRIBUTING.md
# §"Bar for any change" for the full gate-by-gate map.

# The complete PR bar — `ci` plus the real-server e2e and agent smoke lanes.
ci-full: ci e2e agents-fleet-smoke
    @echo "ok (full)"

# Print the toolchain we are pinned to.
toolchain:
    @rustc --version
    @cargo --version

# Package the host-target release binaries into a tarball matching the
# release workflow's naming (phux-<tag>-<target>.tar.gz) under dist/. Used
# to seed the first Homebrew release locally; CI does this per-target on a
# `v*` tag. Pass the tag, e.g. `just dist v0.0.1`.
dist TAG:
    bash scripts/dist.sh {{TAG}}

# Local release preflight before pressing the GitHub Actions release button.
# Runs version/tag checks, install-surface drift checks, formula generation,
# and a phux-protocol crates.io package dry-run.
release-preflight TAG:
    bash scripts/release-preflight.sh {{TAG}}

# Same release preflight, but skip the crates.io dry-run when offline or when
# this is a binary/Homebrew-only release and cargo registry access is flaky.
release-preflight-fast TAG:
    bash scripts/release-preflight.sh {{TAG}} --skip-crate-dry-run

# Check that a release tag matches the resolved Cargo package versions.
release-check TAG:
    bash scripts/check-release-version.sh {{TAG}}

# Dry-run the crates.io publish of phux-protocol (package + verify, no
# upload). The only publishable crate. Mirrors the publish-crate workflow.
publish-protocol-dry:
    cargo publish --locked --dry-run -p phux-protocol

# Publish phux-protocol to crates.io. IRREVERSIBLE. Requires `cargo login`
# (or CARGO_REGISTRY_TOKEN). Run `just publish-protocol-dry` first.
publish-protocol:
    cargo publish --locked -p phux-protocol

# Builds the `profiling` profile (release codegen + line-table debug
# info) then records a Firefox Profiler JSON at target/samply-profile.json.
# Default subcommand is `server`; pass any other subcommand + args:
#
#   just profile                 # records `phux server`
#   just profile attach default  # records `phux attach default`
#
# samply is not a workspace dep — install with `cargo install samply`.

# CPU-profile the phux binary with samply.
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

# --- Build observability ---------------------------------------------------
# Three lenses on "why is the build this slow / this big". Honest caveat:
# the dominant COLD-build cost is libghostty-vt's zig blob (a build.rs
# shell-out to zig), which none of these three see — they profile the Rust
# side. For the zig cost, lean on the CPU-keyed CI cache and not rebuilding
# per-worktree. These tools find the *Rust* wins: critical-path crates,
# monomorphization bloat, and binary size.

# Cargo's built-in compile-time report -> target/cargo-timings/. Shows the
# per-crate timeline, the critical path (the longest dependency chain
# gating everything else), and the codegen-vs-frontend split. Pass extra
# args to change profile:
#   just timings                 # debug, --all-targets (dev iteration cost)
#   just timings --release       # the release/LTO timeline
# CAVEAT: a WARM build's timeline is near-empty because cached crates don't
# recompile. For a true cold picture, `cargo clean` first. There is no CI
# lane that builds cold for you any more (ADR-0082) — this is the tool.

# HTML compile-time report (critical path, codegen vs frontend).
timings *ARGS:
    cargo build --workspace --all-targets --timings {{ARGS}}
    @echo "report -> target/cargo-timings/cargo-timing.html"

# LLVM IR lines emitted per (generic) function for one crate — the
# monomorphization-bloat view. A helper instantiated for hundreds of type
# combinations shows up at the top; the fix is usually `#[inline(never)]`
# or pulling the type-independent body into a non-generic fn. Reads the
# `llvm-tools-preview` component (pinned in rust-toolchain.toml).
#   just llvm-lines                     # phux-protocol lib (default)
#   just llvm-lines phux-server         # another crate's lib
#   just llvm-lines phux --bin phux     # a specific binary target

# Per-function LLVM IR line counts (monomorphization bloat) for one crate.
llvm-lines PKG='phux-protocol' *ARGS:
    cargo llvm-lines -p {{PKG}} {{ARGS}}

# Attribute release binary size. Defaults to a by-crate breakdown of the
# `phux` binary; pass args for the per-function view or another target:
#   just bloat                   # size by crate (the phux binary)
#   just bloat -n 30             # top 30 individual functions
#   just bloat --bin phux-mcp    # a different binary

# Attribute release binary size by crate (or per-fn with args).
bloat *ARGS:
    cargo bloat --release --bin phux {{ if ARGS == "" { "--crates" } else { ARGS } }}

# Dependency-graph stats without compiling: locked-package count, duplicate
# versions (each compiles separately in cold CI), proc-macro and
# build-script crate counts. Prints markdown to stdout.
dep-stats:
    bash scripts/ci/dep-stats.sh
