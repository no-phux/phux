# Unit, e2e, stress, and example smokes. Keep --workspace feature union
# aligned across test/e2e/stress (CONTRIBUTING.md).


# Test-lane only: bound auto-spawned daemons (ADR-0063 keeps production
# unattended servers up). Lanes that `env_clear()` re-arm from
# `AutoSpawnedServer::IDLE_BACKSTOP` instead. Do not export this justfile-wide.
AUTO_SPAWN_BACKSTOP := "PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE=600"


# Workspace unit pool via nextest. Same --workspace feature union as e2e/stress
# (CONTRIBUTING.md). Optional profiler surfaces compile under lint/doc instead.
[group('test')]
[doc('Run the workspace unit test pool via nextest.')]
test:
    #!/usr/bin/env bash
    set -euo pipefail
    # Build selection stays --workspace for main and for library changes (see
    # the e2e note). A tests/-only PR sets PHUX_UNIT_MODE=narrow and builds
    # that integration test, not the other binaries. rdeps still only filters
    # execution after the workspace build.
    if [[ "${PHUX_UNIT_MODE:-workspace}" == "narrow" ]]; then
      IFS=',' read -ra specs <<< "${PHUX_UNIT_TARGETS:?PHUX_UNIT_TARGETS is required}"
      for spec in "${specs[@]}"; do
        pkg="${spec%%:*}"
        bin="${spec##*:}"
        if [[ "$bin" == "*" ]]; then
          {{AUTO_SPAWN_BACKSTOP}} cargo nextest run -p "$pkg" --tests
        else
          {{AUTO_SPAWN_BACKSTOP}} cargo nextest run -p "$pkg" --test "$bin"
        fi
      done
      exit 0
    fi
    if [[ "${PHUX_UNIT_MODE:-workspace}" == "skip" ]]; then
      echo "unit pool skipped: no library or integration-test inputs changed"
      exit 0
    fi
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


# Stable-cargo test for environments without nextest.
[group('test')]
test-cargo:
    {{AUTO_SPAWN_BACKSTOP}} cargo test --workspace
