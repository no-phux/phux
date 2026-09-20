# Local observability. Advisory; does not route product CI lanes.


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
