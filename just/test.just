# Unit, e2e, stress, and example smokes. Keep --workspace feature union
# aligned across test/e2e/stress (CONTRIBUTING.md).


# Test-lane only: bound auto-spawned daemons (ADR-0063 keeps production
# unattended servers up). Lanes that `env_clear()` re-arm from
# `AutoSpawnedServer::IDLE_BACKSTOP` instead. Do not export this justfile-wide.
AUTO_SPAWN_BACKSTOP := "PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE=600"


# Workspace unit pool via nextest. Same --workspace feature union as e2e/stress
# (CONTRIBUTING.md). Optional profiler surfaces compile under lint/doc instead.
#
# CI's plan sets the scope: PHUX_UNIT_MODE (workspace, rdeps, narrow, skip),
# PHUX_UNIT_TARGETS for narrow, and PHUX_NEXTEST_FILTERSET for rdeps.
# PHUX_TEST_ARCHIVE names a `just test-archive` output to run without
# compiling; its e2e binaries hold only #[ignore]d tests, so they select none.
[group('test')]
[doc('Run the workspace unit test pool via nextest.')]
test: worktree-claim && (worktree-verify "test")
    #!/usr/bin/env bash
    set -euo pipefail
    archive="${PHUX_TEST_ARCHIVE:-}"
    # Build selection stays --workspace for main and for library changes (see
    # the e2e note). A tests/-only PR sets PHUX_UNIT_MODE=narrow and builds
    # that integration test, not the other binaries. rdeps only filters
    # execution after the build.
    if [[ "${PHUX_UNIT_MODE:-workspace}" == "narrow" ]]; then
      IFS=',' read -ra specs <<< "${PHUX_UNIT_TARGETS:?PHUX_UNIT_TARGETS is required}"
      if [[ -n "$archive" ]]; then
        filter=""
        for spec in "${specs[@]}"; do
          IFS=':' read -r pkg bin _ <<< "$spec"
          if [[ "$bin" == "*" ]]; then one="(package(=${pkg}) & kind(test))"; else one="binary_id(${pkg}::${bin})"; fi
          filter="${filter:+${filter} + }${one}"
        done
        # Gated targets hold `#[ignore]`d tests only, so building them is the check.
        {{AUTO_SPAWN_BACKSTOP}} cargo nextest run --archive-file "$archive" \
          --workspace-remap "{{justfile_directory()}}" --no-tests=warn -E "$filter"
        exit 0
      fi
      # A spec is `package:binary`, plus `:feature+feature` when the target
      # is gated behind non-default `required-features`. Gated targets hold
      # `#[ignore]`d tests only, so building them is the check.
      for spec in "${specs[@]}"; do
        IFS=':' read -r pkg bin feats <<< "$spec"
        features=()
        if [[ -n "$feats" ]]; then features=(--features "${feats//+/,}" --no-tests=warn); fi
        if [[ "$bin" == "*" ]]; then
          {{AUTO_SPAWN_BACKSTOP}} cargo nextest run -p "$pkg" --tests
        else
          {{AUTO_SPAWN_BACKSTOP}} cargo nextest run -p "$pkg" --test "$bin" ${features[@]+"${features[@]}"}
        fi
      done
      exit 0
    fi
    if [[ "${PHUX_UNIT_MODE:-workspace}" == "skip" ]]; then
      echo "unit pool skipped: no library or integration-test inputs changed"
      exit 0
    fi
    filter="${PHUX_NEXTEST_FILTERSET:-}"
    if [[ -n "$archive" ]]; then
      source=(--archive-file "$archive" --workspace-remap "{{justfile_directory()}}")
    else
      source=(--workspace {{TEST_FEATURES}})
    fi
    extra=()
    if [[ -n "$filter" ]]; then extra+=(-E "$filter"); fi
    # An rdeps filterset can select nothing; that is a pass, not an error.
    {{AUTO_SPAWN_BACKSTOP}} cargo nextest run "${source[@]}" --no-tests=warn ${extra[@]+"${extra[@]}"}

# `phux-server/measure` builds the e2e lane's `perf` target, which a
# default-feature build skips. The feature is empty: it changes no code and no
# dependency's feature set, so the unit pool and e2e share one build.
TEST_FEATURES := "--features phux-server/measure"


# Compile every target `just test` and `just e2e` run into one nextest archive
# (PHUX_TEST_ARCHIVE, default target/nextest/phux-tests.tar.zst) for CI's
# tests and e2e jobs. It holds the test binaries, the `phux` and `phux-mcp` executables the
# tests spawn, and the libraries those link at run time. PHUX_UNIT_MODE=narrow
# builds only PHUX_UNIT_TARGETS, plus the e2e set unless PHUX_E2E=false.
[group('test')]
[doc('Build the unit and e2e test binaries into one nextest archive.')]
test-archive: worktree-claim
    #!/usr/bin/env bash
    set -euo pipefail
    archive="${PHUX_TEST_ARCHIVE:-{{justfile_directory()}}/target/nextest/phux-tests.tar.zst}"
    mkdir -p "$(dirname "$archive")"
    targets=(--lib --bins --tests)
    features=()
    if [[ "${PHUX_UNIT_MODE:-workspace}" == "narrow" ]]; then
      targets=()
      IFS=',' read -ra specs <<< "${PHUX_UNIT_TARGETS:?PHUX_UNIT_TARGETS is required}"
      for spec in "${specs[@]}"; do
        IFS=':' read -r pkg bin feats <<< "$spec"
        if [[ -n "$feats" ]]; then features+=(--features "${pkg}/${feats//+/,${pkg}/}"); fi
        if [[ "$bin" == "*" ]]; then
          while IFS= read -r name; do targets+=(--test "$name"); done < <(
            cargo metadata --no-deps --format-version 1 | jq -r --arg pkg "$pkg" '
              .packages[] | select(.name == $pkg) | (.features.default // []) as $default
              | .targets[] | select(.kind == ["test"])
              | select(((."required-features" // []) - $default) == []) | .name')
        else
          targets+=(--test "$bin")
        fi
      done
      if [[ "${PHUX_E2E:-true}" == "true" ]]; then targets+=({{E2E_TARGETS}}); fi
    fi
    cargo nextest archive --workspace {{TEST_FEATURES}} ${features[@]+"${features[@]}"} \
      "${targets[@]}" --archive-file "$archive"


# Fast e2e + perf gates (every PR). #[ignore]d out of `just test`; serial +
# retries because these spin a real server+PTY. Keep --workspace so the
# feature union matches `test` (a `-p` selection recompiles tokio). Filterset
# names binaries; `scripts/check-e2e-lanes.sh` (in `just guards`) fails if an ignored *_e2e.rs is in
# no lane. Flag-stability story: CONTRIBUTING.md.
#
# The `--test` targets limit a local build to the binaries the filtersets
# name, so it does not compile the unit pool's lib-test harnesses. MCP's
# `discovery` test is built, not run: it makes Cargo build the phux-mcp
# executable in this same graph, and first_five_minutes_e2e copies both
# payload binaries. PHUX_TEST_ARCHIVE runs the same filtersets from a
# `just test-archive` output instead.
[group('test')]
[doc('Fast e2e + perf gates that spin a real server; every PR.')]
e2e: worktree-claim && (worktree-verify "e2e")
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -n "${PHUX_TEST_ARCHIVE:-}" ]]; then
      source=(--archive-file "$PHUX_TEST_ARCHIVE" --workspace-remap "{{justfile_directory()}}")
    else
      source=(--workspace {{TEST_FEATURES}} {{E2E_TARGETS}})
    fi
    {{AUTO_SPAWN_BACKSTOP}} cargo nextest run "${source[@]}" --run-ignored ignored-only \
      --test-threads=1 --retries=2 \
      -E 'binary_id(phux::automation_e2e) + binary_id(phux::terminal_e2e) + binary_id(phux::recording_e2e) + binary_id(phux::lifecycle_e2e) + binary_id(phux::first_five_minutes_e2e) + binary_id(phux::upload_e2e)'
    {{AUTO_SPAWN_BACKSTOP}} cargo nextest run "${source[@]}" --run-ignored ignored-only \
      --test-threads=1 --retries=2 \
      -E 'binary_id(phux-server::perf)'

E2E_TARGETS := "--test automation_e2e --test terminal_e2e --test recording_e2e --test lifecycle_e2e --test first_five_minutes_e2e --test upload_e2e --test perf --test discovery"


# Heavy stress storms — off CI entirely: starvation-sensitive on 2-core
# runners, so run them locally on a multi-core box before risky merges. Also hosts
# perf_bursty_output (~110s, allocation count, not wall time). The `stress`
# feature builds the target, which the default test build skips.
[group('test')]
[doc('Heavy stress storms; local only, off CI.')]
stress: worktree-claim
    cargo nextest run --workspace --run-ignored ignored-only \
      {{TEST_FEATURES}} --features phux-server/stress --test-threads=1 --retries=2 \
      -E 'binary_id(phux-server::stress)'


# Spins a real `phux` server + session, drives a scripted scenario (heavy
# colored output, a 2nd client attach, a resize storm, an input line) and
# writes screen snapshots + a summary to /tmp/phux-repro-<ts>/ for
# inspection. See crates/phux-server/examples/e2e-repro.rs.
#
# One-command real-server repro of a lag/crash edge case.
[group('test')]
e2e-repro: worktree-claim
    cargo run -p phux-server --example e2e-repro


# Hermetic argv/control-flow gate for the placed-fleet worked example. Uses a
# fake phux binary, so it needs neither a server nor installed agent CLIs.
[group('test')]
[doc('Hermetic argv and control-flow gate for the placed-fleet example.')]
agents-fleet-smoke: worktree-claim
    bash examples/agents/tests/placed-fleet-smoke.sh


# Real isolated server dogfood for placement/layout/watch/ask with shell panes;
# no external agent binary is needed. Set PHUX_DOGFOOD_REAL_AGENTS=1 to also
# spawn installed claude/codex binaries on the private server.
[group('test')]
[doc('Real isolated-server dogfood for placement, layout, watch, and ask.')]
agents-fleet-live: worktree-claim
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
    demo() { cargo run -q -p phux --bin phux -- "$@"; }
    demo config plugins
    demo config plugins --json
    demo config run com.phux.demo.agent-tools inspect
    demo config run com.phux.demo.agent-tools inspect --json
    demo config run com.phux.demo.agent-tools list-integrations
    demo config run com.phux.demo.agent-tools validate-integrations
    demo config run com.phux.demo.agent-tools status-integrations
    demo config run com.phux.demo.agent-tools smoke-integrations
    demo config run com.phux.demo.agent-tools detect-agents


# Stable-cargo test for environments without nextest.
[group('test')]
test-cargo: worktree-claim
    {{AUTO_SPAWN_BACKSTOP}} cargo test --workspace
