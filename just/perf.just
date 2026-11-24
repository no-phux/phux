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
    PHUX_LOG="$log" PHUX_LOG_FORMAT=json RUST_LOG="{{level}}" cargo run -q -p phux --bin phux -- attach {{session}} || true
    echo "[trace] session ended -> hand off this file: $log"
    echo "[trace] quick peek at the slowest renders:"
    jq -rc 'select(.fields.message=="close" and (.span.name|test("render|handle_server_frame|synthesize|tick_emit"))) | [.fields["time.busy"], .span.name, (.span.changed_row_count//.span.out_bytes//"")] | @tsv' "$log" 2>/dev/null | sort -h | tail -15 || true


# Live performance telemetry of the running server (ADR-0096). One row per
# hot-path metric, one interval per second; Ctrl-C to stop. Same as
# `phux perf --watch 1` on the installed binary, built from this tree.
[group('perf')]
[doc('Live performance telemetry of the running server (ADR-0096).')]
perf interval="1":
    cargo run -q -p phux --bin phux -- perf --watch {{interval}}


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

# Rune's keystroke-echo hops (clients/rune/DEVELOP.md#keystroke-latency):
# headless phux-rune typing into an isolated release server, frames paced at
# `fps`. Prints p50/p99 per hop and key event to drawn frame.
#   just perf-rune-echo             # 60 Hz pacing, 200 keys
#   just perf-rune-echo 1000        # client and server work alone
[group('perf')]
[doc('Rune keystroke-echo hops against an isolated release server.')]
perf-rune-echo fps="60" keys="200":
    cargo build --release -p phux
    cargo build --release --locked --manifest-path clients/rune/Cargo.toml
    bash clients/rune/bench/rune-echo.sh target/release/phux clients/rune/target/release/phux-rune {{keys}} 120 {{fps}}

# Key to on-screen pixels, Rune against Ghostty on the same display (macOS;
# Screen Recording, Accessibility, an unlocked screen and uncovered windows).
[group('perf')]
[doc('Rune vs Ghostty key-to-pixels latency on this Mac.')]
perf-rune-photon keys="200":
    cargo build --release -p phux
    cargo build --release --locked --manifest-path clients/rune/Cargo.toml
    bash clients/rune/bench/rune-echo.sh --photon target/release/phux clients/rune/target/release/phux-rune {{keys}} 250


# Cyclomatic complexity of Rust source over a CCN ceiling — advisory.
# clippy.toml is the CI gate; this is lizard via uvx. Not in `ci`.
[group('perf')]
complexity CCN="15":
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v uvx >/dev/null 2>&1; then
      echo "complexity: uvx not found; install uv to run this report" >&2
      exit 1
    fi
    # Warnings are data, not a lizard failure. Do not mask broken invocations.
    warnings="$(uvx --from lizard==1.24.3 lizard -l rust crates --CCN {{CCN}} --length 1000000 -w -i -1)"
    if ! grep -vE '/(tests|benches|examples)/' <<<"$warnings" | grep -F 'warning:'; then
      echo "no source function exceeds CCN {{CCN}}"
    fi


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
timings *ARGS: worktree-claim
    cargo build --timings {{ if ARGS == "" { "--workspace --all-targets" } else { ARGS } }}
    @echo "report -> target/cargo-timings/cargo-timing.html"


[group('perf')]
llvm-lines PKG='phux-protocol' *ARGS:
    cargo llvm-lines -p {{PKG}} {{ARGS}}


[group('perf')]
bloat *ARGS:
    cargo bloat --release --bin phux {{ if ARGS == "" { "--crates" } else { ARGS } }}


# Callgrind instruction counts for the hot paths, compared with
# benchmarks/icount-baseline.json. Run inside `nix develop` on x86_64.
# `--update` rewrites the baseline; see docs/development.md#performance before
# committing that file.
#
# Builds phux-server's `icount` and phux-terminal's `terminal_icount` benches
# with the `icount` profile and runs them under callgrind, then compares their
# combined Ir to the baseline. Valgrind comes from the flake's nixpkgs so its
# libc matches the devshell that built the bench.
[group('perf')]
[doc('Callgrind instruction counts against benchmarks/icount-baseline.json.')]
[positional-arguments]
perf-icount *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    case "$*" in
      "") action=compare ;;
      --update) action=update ;;
      *)
        echo "usage: just perf-icount [--update]" >&2
        exit 2
        ;;
    esac
    root="{{justfile_directory()}}"
    # Pure git policy: refuse an unauthorized baseline edit before the slow bench.
    bun tools/main.ts perf-icount authorize
    if ! command -v nix >/dev/null 2>&1; then
      echo "perf-icount: nix is required so valgrind matches the devshell libc" >&2
      exit 1
    fi
    valgrind_bin="$(nix shell --inputs-from "$root" nixpkgs#valgrind -c bash -c 'command -v valgrind')"
    PATH="$(dirname "$valgrind_bin"):${PATH}"
    # The bench spawns iai-callgrind-runner, whose version must match the
    # iai-callgrind crate in Cargo.lock.
    runner_version="$(bun tools/main.ts perf-icount runner-version)"
    runner_root="${root}/target/icount-runner"
    if [[ ! -x ${runner_root}/bin/iai-callgrind-runner ]]; then
      cargo install --locked --version "${runner_version}" --profile release \
        --target-dir "${root}/target/icount-runner-build" --root "${runner_root}" iai-callgrind-runner
    fi
    export PATH="${runner_root}/bin:${PATH}"
    out="${root}/target/perf-icount"
    mkdir -p "$out"
    : > "${out}/iai.jsonl"
    cargo build --locked --profile icount \
      -p phux-server --bench icount -p phux-terminal --bench terminal_icount
    for bench in icount terminal_icount; do
      # Newest matching file: incremental builds leave earlier hashes beside it.
      executable="$(
        find "${root}/target/icount/deps" -type f -name "${bench}-*" -executable ! -name '*.d' -printf '%T@\t%p\n' \
          | sort -n | tail -1 | cut -f2-
      )"
      if [[ -z ${executable} || ! -x ${executable} ]]; then
        echo "perf-icount: ${bench} bench executable not found under target/icount/deps" >&2
        exit 1
      fi
      "$executable" --output-format=json --home="${out}/iai" >> "${out}/iai.jsonl"
    done
    bun tools/main.ts perf-icount "$action" "${out}/iai.jsonl"


# Dependency-graph stats without compiling: counts and shapes that explain
# build cost. Reads `cargo metadata` (locked) and prints markdown to the step
# summary, or to stdout locally.
#   locked_packages   every crate CI may compile cold
#   duplicate crates  same crate, multiple versions; each compiles separately
#   proc_macros       compile serially early and gate everything downstream
#   build_scripts     each is a compile + an execution (libghostty-vt's zig
#                     shell-out is the workspace's dominant single cost)
[group('perf')]
[doc('Dependency-graph stats without compiling: counts, duplicates, proc-macros.')]
dep-stats:
    #!/usr/bin/env bash
    set -euo pipefail
    meta=$(cargo metadata --format-version 1 --locked 2>/dev/null || cargo metadata --format-version 1)
    stats=$(jq -c \
        '(.workspace_members) as $ws
         | (.packages | map(select(.id as $id | $ws | index($id)))) as $members
         | ($members | map(.name)) as $member_names
         | {workspace_members: ($members | length),
            locked_packages: (.packages | length),
            direct_deps: ([$members[].dependencies[].name] | unique
                          | map(select(. as $n | $member_names | index($n) | not))
                          | length),
            proc_macros: ([.packages[] | select(any(.targets[]; .kind | index("proc-macro")))] | length),
            build_scripts: ([.packages[] | select(any(.targets[]; .kind | index("custom-build")))] | length),
            duplicates: (.packages | map(select(.name as $n | $member_names | index($n) | not))
                         | group_by(.name) | map(select(length > 1)
                         | {name: .[0].name, versions: [.[].version]}))}' \
        <<<"$meta")
    {
        echo "### dependency graph"
        echo
        jq -r '"- locked packages: **\(.locked_packages)** (\(.workspace_members) workspace members, \(.direct_deps) direct deps)
    - proc-macro crates: \(.proc_macros); crates with build scripts: \(.build_scripts)
    - duplicate versions: **\(.duplicates | length)**"' <<<"$stats"
        if jq -e '.duplicates | length > 0' <<<"$stats" >/dev/null; then
            echo
            echo "| duplicated crate | versions |"
            echo "|---|---|"
            jq -r '.duplicates[] | "| `\(.name)` | \(.versions | join(", ")) |"' <<<"$stats"
        fi
        echo
    } >>"${GITHUB_STEP_SUMMARY:-/dev/stdout}"
