---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-09-30
---

# Performance

**TL;DR.** Fresh two-run measurements compare real phux, tmux, and Herdr clients
on one heavily loaded development host. Results change substantially between
runs: these are on-host observations, not an idle-machine ranking.
Raw samples, failures, and exact boundaries are retained. cmux's native GUI
requires a separate safe test session; it is not assigned a fabricated score.

## What can be compared fairly?

| Product | Local terminal-client experiment | Native GUI experiment |
|---|---|---|
| **phux** | Server plus `phux attach`; UDS locally, optional loopback WebSocket and QUIC lanes | Cockpit, desktop, and browser clients are **not measured** by the TUI harness. |
| **tmux** | Private tmux server plus `tmux attach-session`; no user configuration | The outer terminal application is **not measured**. |
| **Herdr** | Private Herdr server plus its terminal UI | No GUI timing is inferred from the terminal-client result. |
| **cmux** | Its native app is **not applicable** to this PTY-client probe. Running its CLI, a bare shell, or its optional tmux owner would measure a different boundary. | Requires an isolated native app, owned workspace, and an explicit completion signal; no input-to-pixel result is currently published here. |

**Unmeasured is not slow, unsupported, or zero.** Choose a product using
[When to use phux](./when-to-use.md); use this page to inspect the cost of a
specific workflow. A smaller server RSS does not imply a smaller complete app.

## Recorded-load observations: September 30, 2026

**Do not read these as an idle-host baseline.** The machine remained a shared
development host. Work for this comparison was serialized, but unrelated
background load was not stopped or controlled. The one-, five-, and
fifteen-minute load averages at the start of each run were:

| Run | Product order | Start (UTC) | Load averages |
|---|---|---|---|
| 1 | phux → Herdr → tmux | 05:16:03 | 77.33 / 218.72 / 172.60 |
| 2 | tmux → Herdr → phux | 05:23:51 | 107.23 / 141.02 / 152.56 |

Hardware: Apple M4 Pro, 14 logical CPUs, 48 GiB RAM, arm64; macOS 27.0 build
`26A428`. Power settings observed during the campaign: AC power, `powermode 0`.
The load readings are start-of-run snapshots, not continuous measurements.

Tested binaries: **phux `0.46.0+next.c6f83ef` over local UDS**, **tmux `3.7c`**,
and **Herdr `0.9.0`**. Exact binary hashes and harness revision
`6ad7bebb6f46558f5db8cd78f8041f5b38eec150` are retained in each run's metadata;
the [measured harness snapshot](https://github.com/no-phux/phux/tree/bench/docs-2026-09-30/scripts/bench)
is preserved separately from later documentation changes.
No fresh remote-network, WebSocket, QUIC, or native GUI result is implied.

### Both runs, without selecting a winner

Each cell shows **run 1 / run 2**. Lower timings are shorter observations at the
stated boundary, not proof of a generally faster product.

<div class="benchmark-charts">
<figure class="benchmark-chart">
<figcaption><strong>PTY-byte echo, median</strong><span>Microseconds · 1,000 samples per bar · not pixels</span></figcaption>
<div class="benchmark-group">
<p>phux</p>
<div class="benchmark-measure"><span>Run 1</span><div aria-hidden="true"><i style="width:96.7%"></i></div><span>1,934 µs</span></div>
<div class="benchmark-measure benchmark-repeat"><span>Run 2</span><div aria-hidden="true"><i style="width:13.9%"></i></div><span>278 µs</span></div>
</div>
<div class="benchmark-group">
<p>tmux</p>
<div class="benchmark-measure"><span>Run 1</span><div aria-hidden="true"><i style="width:7.25%"></i></div><span>145 µs</span></div>
<div class="benchmark-measure benchmark-repeat"><span>Run 2</span><div aria-hidden="true"><i style="width:40.35%"></i></div><span>807 µs</span></div>
</div>
<div class="benchmark-group">
<p>Herdr</p>
<div class="benchmark-measure"><span>Run 1</span><div aria-hidden="true"><i style="width:18.25%"></i></div><span>365 µs</span></div>
<div class="benchmark-measure benchmark-repeat"><span>Run 2</span><div aria-hidden="true"><i style="width:25.05%"></i></div><span>501 µs</span></div>
</div>
<p class="benchmark-scale">Linear scale: 0–2,000 µs. Lower is less delay.</p>
</figure>
<figure class="benchmark-chart">
<figcaption><strong>Warm attach, median</strong><span>Milliseconds · 20 samples per bar</span></figcaption>
<div class="benchmark-group">
<p>phux</p>
<div class="benchmark-measure"><span>Run 1</span><div aria-hidden="true"><i style="width:94%"></i></div><span>188 ms</span></div>
<div class="benchmark-measure benchmark-repeat"><span>Run 2</span><div aria-hidden="true"><i style="width:29%"></i></div><span>58 ms</span></div>
</div>
<div class="benchmark-group">
<p>tmux</p>
<div class="benchmark-measure"><span>Run 1</span><div aria-hidden="true"><i style="width:32%"></i></div><span>64 ms</span></div>
<div class="benchmark-measure benchmark-repeat"><span>Run 2</span><div aria-hidden="true"><i style="width:55.5%"></i></div><span>111 ms</span></div>
</div>
<div class="benchmark-group">
<p>Herdr</p>
<div class="benchmark-measure"><span>Run 1</span><div aria-hidden="true"><i style="width:79.5%"></i></div><span>159 ms</span></div>
<div class="benchmark-measure benchmark-repeat"><span>Run 2</span><div aria-hidden="true"><i style="width:84.5%"></i></div><span>169 ms</span></div>
</div>
<p class="benchmark-scale">Linear scale: 0–200 ms. Lower is less delay.</p>
</figure>
</div>

The same measurements are tabulated below. Both charts retain both runs;
the change in their ordering is the finding, not a reason to discard a run.

| Measurement | phux | tmux | Herdr |
|---|---:|---:|---:|
| PTY-byte echo p50, µs | 1,934 / 278 | 145 / 807 | 365 / 501 |
| PTY-byte echo p99, µs | 15,876 / 1,272 | 1,843 / 20,010 | 23,247 / 43,564 |
| PTY-byte echo maximum, µs | 23,537 / 3,226 | 3,900 / 90,004 | 93,304 / 60,955 |
| Cold-server attach median, ms; server start excluded | 210 / 59 | 83 / 105 | 379 / 108 |
| Warm attach median, ms | 188 / 58 | 64 / 111 | 159 / 169 |
| 300,000 lines to captured marker, ms | 2,470 / 367 | 547 / 2,610 | 867 / 367 |
| Server-only RSS after output, KiB | 18,912 / 18,064 | 4,080 / 5,072 | 31,040 / 30,480 |
| Attach-client-only RSS after output, KiB | 26,224 / 16,192 | 5,152 / 5,152 | 15,760 / 16,336 |
| Loaded history: warm attach median, ms | 162 / 104 | 56 / 63 | 174 / 163 |
| Loaded history: PTY-byte echo p50, µs | 289 / 247 | 145 / 166 | **probe failed** / 873 |
| Loaded history: PTY-byte echo p99, µs | 5,930 / 1,202 | 5,136 / 1,201 | **probe failed** / 19,893 |

There are **20 attach samples per phase per product per run**, with no attach
timeouts. Every successful echo probe completed **1,000 iterations with no echo
timeouts**; the loaded-history second client starts at iteration 500. Bulk,
RSS, idle, and resize are single observations per product per run, not
distributions. The full JSON contains their remaining rows and raw samples.

In run 1, Herdr's loaded-history probe saw a prompt but could not make the pane
answer its readiness command. Its four history-fill completion markers were
observed; the echo timing phase did not start. Run 2 succeeded without changing
the harness. This is an **unresolved readiness failure**, not zero latency, a
five-second echo sample, or proof that Herdr itself cannot handle the workload.

The honest conclusions are narrower than a leaderboard:

- phux's fresh echo median was higher than both peers in run 1 and lower than
  both in run 2. The reversal makes a stable speed ranking indefensible here.
- tmux had the lowest measured server RSS and loaded-history attach median in
  both runs. Its default retained history and feature boundary differ from the
  other products, so this is not memory-per-retained-byte efficiency.
- phux's server RSS was below Herdr's in both runs; client RSS did not show the
  same consistent advantage. Server-only memory is not total application cost.
- The broad spread between medians, p99s, and maxima is part of the evidence.
  It is not removed by reporting only the more flattering run.

### Download and repeat

| Evidence | Run 1 | Run 2 |
|---|---|---|
| Versions, hashes, host and exact invocation | [metadata](https://docs.phux.sh/benchmarks/2026-09-30/run1/metadata.json) | [metadata](https://docs.phux.sh/benchmarks/2026-09-30/run2/metadata.json) |
| All result rows and raw sample arrays | [samples](https://docs.phux.sh/benchmarks/2026-09-30/run1/samples.json) | [samples](https://docs.phux.sh/benchmarks/2026-09-30/run2/samples.json) |
| Server/client command record | [commands](https://docs.phux.sh/benchmarks/2026-09-30/run1/commands.txt) | [commands](https://docs.phux.sh/benchmarks/2026-09-30/run2/commands.txt) |
| phux echo / loaded history | [echo](https://docs.phux.sh/benchmarks/2026-09-30/run1/phux-pty-echo.json) · [history](https://docs.phux.sh/benchmarks/2026-09-30/run1/phux-bh-pty-echo.json) | [echo](https://docs.phux.sh/benchmarks/2026-09-30/run2/phux-pty-echo.json) · [history](https://docs.phux.sh/benchmarks/2026-09-30/run2/phux-bh-pty-echo.json) |
| tmux echo / loaded history | [echo](https://docs.phux.sh/benchmarks/2026-09-30/run1/tmux-pty-echo.json) · [history](https://docs.phux.sh/benchmarks/2026-09-30/run1/tmux-bh-pty-echo.json) | [echo](https://docs.phux.sh/benchmarks/2026-09-30/run2/tmux-pty-echo.json) · [history](https://docs.phux.sh/benchmarks/2026-09-30/run2/tmux-bh-pty-echo.json) |
| Herdr echo / loaded history (failure retained) | [echo](https://docs.phux.sh/benchmarks/2026-09-30/run1/herdr-pty-echo.json) · [history](https://docs.phux.sh/benchmarks/2026-09-30/run1/herdr-bh-pty-echo.json) | [echo](https://docs.phux.sh/benchmarks/2026-09-30/run2/herdr-pty-echo.json) · [history](https://docs.phux.sh/benchmarks/2026-09-30/run2/herdr-bh-pty-echo.json) |

Publication replaces local binary, repository, and temporary paths with named
placeholders; versions, hashes, measured values, and samples are unchanged.
Original logs are retained separately. The
[smoke metadata](https://docs.phux.sh/benchmarks/2026-09-30/smoke/metadata.json)
records the preceding three-sample functional check; its numbers are not used
as performance evidence. The full repeat command is below; use the recorded
binary versions and reverse `--mux` order for the second run.

## Historical result: September 2, 2026

These are the previously published observations, retained as historical context.
The raw September 2 directory was not retained, and the original per-metric
sample counts cannot be verified. Consequently these numbers cannot be
recalculated, given confidence intervals, or used as a current-release baseline.

The run used phux `0.23.3`, source revision
`c90e2f33cfece36c9566d6949a3ef15d8f5b078f`, and Herdr `0.8.2` on an Apple M4 Pro,
48 GiB RAM, macOS 27.0. The outer terminal was 120×40; the loaded-history fixture
used four 188×40 terminal panes, each fed 60,000 lines. Servers used separate
HOME, XDG directories, and sockets. All transports were local, including QUIC
and WebSocket; this was not an Internet latency test.

| Historical observation | phux | Herdr |
|---|---:|---:|
| PTY-byte key echo, p50 | 176 µs (UDS) | 791 µs |
| Server-only RSS after 300,000 output lines | 28.7 MB (UDS) | 33.3 MB |
| 300,000 lines to captured completion marker | 375 ms (WebSocket) | not recorded |
| Loaded history: warm reattach median | 117 ms | **85 ms** |
| Loaded history: PTY-byte key echo, p50 | 174 µs | 12,447 µs |
| Loaded history: server-only RSS with a client attached | 46 MB | not recorded |

phux's lower echo median did **not** mean it won every workflow: Herdr reattached
faster with loaded history. The historical phux-only attach observations were
64 ms cold UDS, 60 ms cold QUIC, 70 ms warm UDS, and 1.07 ms from UDS connection to
the logged `ATTACHED` response. Protocol acknowledgement and usable UI are
separate milestones; do not compare 1.07 ms with another product's full attach.
The old RSS values retain their originally reported MB unit, rather than
silently reinterpreting them as MiB.

### Correction to the old tmux label

The old `mux-compare.sh` lane named `tmux` launched `/bin/sh` for byte-level echo.
That was a **bare-PTY baseline, not a tmux measurement**. The separate tmux
observer used for screen capture did not put tmux into that byte probe's path.
No historical tmux performance number is claimed here.

The corrected lane starts its own tmux server on a fresh `-S` socket, with
`-f /dev/null`, then runs a real `tmux attach-session` inside the probe PTY. It
also measures attach, bulk output, per-process memory, idle CPU, resize, and
loaded history. The observer and tested tmux server have different sockets.

## Methodology and boundaries

### Key echo: terminal output bytes, not pixels

`scripts/bench/pty-echo.py` creates a controlling PTY at the requested size,
launches the real attach client, proves that the shell answers a command, drains
pending output, and writes one printable character. A monotonic clock stops
when that character returns in the client's escape-stripped output. `Ctrl-U`
clears the input line between iterations. A five-second timeout is recorded as
a failed iteration, not silently discarded as a fast sample.

The path includes the client, transport, server, inner PTY, and shell/terminal
line discipline. It **excludes the outer terminal emulator's rendering,
compositor, display refresh, and physical keyboard**. Escape stripping is a byte
observer, not a full terminal emulator; this is a quiet-shell probe, not a claim
about arbitrary full-screen applications. Report requested samples, completed
samples, and timeouts alongside percentiles.

The default 60 samples are suitable for harness diagnosis, not tail claims. Use
at least 1,000 completed samples for the published empirical p99; the probe
leaves p99 empty below that threshold. This gives roughly ten observations in
the top one percent, not statistical certainty. Retain maximum and all samples,
repeat the entire experiment, and report inter-run variation rather than pooling
away scheduling outliers. The harness uses the sorted sample at the rounded
index `p × (n − 1) / 100`.

### Attach, output, idle, and resize

- **Cold attach:** restart the tested server, wait for its socket, then time
  launching its client until the prompt appears in the observer's captured
  screen. Server launch is **outside** that interval. Warm attach keeps the
  same server and history. Neither is a cold operating-system cache test.
- **Bulk output:** send `seq 1 N` followed by a unique completion marker. Stop
  when the observer captures the marker; the echoed command cannot match it.
  This includes command injection and capture overhead. It does not prove every
  intermediate frame was rendered, or that the entire output was retained.
- **Memory:** `ps` RSS in KiB for the tested server and attach client separately,
  after bulk output. Neither value includes the shell, observer tmux, outer
  terminal, browser helpers, or GPU allocations. Summing RSS can double-count
  shared pages; these are not complete process-tree or physical-footprint totals.
- **Idle CPU:** accumulated server/client CPU-time delta over the sampled wall
  interval. The additional `ps %cpu` value is a decaying-average sanity check,
  not the primary idle result. A zero CPU-time delta is limited by the OS
  accounting resolution, not proof that a process does no work.
- **Resize:** shrink to 100×30, then grow to 120×40; stop after two equal screen
  captures 30 ms apart. This measures a settling heuristic, not frame latency.
- **Loaded history:** feed four terminals 60,000 lines each, observe fresh,
  filled, attached, and detached server RSS, then reattach repeatedly. A second
  client attaches halfway through the echo probe. phux and tmux attach that
  client to `bench2`; Herdr opens another view of its workspace model.

The outer geometry is equal; product chrome can leave different inner terminal
areas. Products retain their default history limits, so equal input is **not**
equal retained history. Loaded-history numbers describe that whole default
configuration, not an engine-per-byte efficiency ranking. Capture-based values
have process-launch and polling overhead; do not use the screen-scrape echo
sanity row for sub-millisecond comparisons. Failed attach samples remain `-1`
in raw data and are excluded from the median, with failure counts shown.

## Run the terminal-client comparison

Prerequisites: Bash 5+, Python 3.11+, tmux, and binaries for the selected lanes.
Build phux separately if needed; let compiler activity stop before measuring.
An already installed binary may be read and executed against a private profile;
never replace it, upgrade its live server, or target its production socket.

```sh
cargo build --locked --release -p phux

# Functional smoke run. Do not publish these small sample counts as performance.
scripts/bench/mux-compare.sh --mux phux,herdr,tmux \
  --phux-bin target/release/phux --herdr-bin /path/to/herdr \
  --tmux-bin /path/to/tmux --attach-samples 1 --pty-iters 3 \
  --key-samples 2 --seq-lines 1000 --out target/bench/comparison-smoke

# Quiet-host measurement. Use a new output directory for every repetition.
scripts/bench/mux-compare.sh --mux phux,herdr,tmux --big-history \
  --phux-bin target/release/phux --herdr-bin /path/to/herdr \
  --tmux-bin /path/to/tmux --attach-samples 20 --pty-iters 1000 \
  --out target/bench/comparison-01
```

Repeat with reversed lane order (`--mux tmux,herdr,phux`) and a new output
directory to expose order/warm-cache effects. Publish each run, including failed
runs. `--mux all` adds phux WebSocket and QUIC; it does **not** mean cmux GUI.
`--rtt-ms`, `--path-mbit`, and `--loss-percent` shape only the QUIC lane using the
local UDP relay. State those parameters when presenting shaped-path results.

The harness launches servers and clients through `env -i`: inherited
`PHUX_WS_*`, `PHUX_SOCKET`, `PHUX_PROFILE`, `TMUX`, and agent credentials are not
forwarded. It assigns private HOME/XDG directories, sockets, and a simple
`/bin/sh` prompt; tmux reads no user configuration. Cleanup only targets
processes and sockets created by this run. Loopback listener collisions fail
the run rather than authorizing use of another server.

### Evidence to retain and publish

Every output directory contains:

- `metadata.json`: UTC timestamp, OS/CPU/RAM, load average, binary paths,
  versions and SHA-256 hashes, harness revision and hashes, exact invocation,
  sample counts, geometry, and measurement boundaries;
- `samples.json`: derived rows plus raw attach/echo/sanity samples;
- `*-pty-echo.json`: command, requested/completed samples, timeout iterations,
  empirical percentiles, and raw microsecond values;
- `commands.txt`, `report.md`, server logs, and per-lane diagnostic directories;
- optional loaded-history, handshake, and shaped-path relay artifacts.

Keep the original directory outside disposable build output before cleaning
`target/`. When publishing a result, commit compact JSON samples and metadata
under `docs/site/public/benchmarks/<run-id>/`, link them from the result, and
retain the original logs privately. Review paths and logs for private hostnames,
tokens, or unrelated output before publication. Binary hashes identify the
artifact actually measured even when a source checkout and installed binary
have different revisions. Never replace a failed run with a success under the
same run ID.

## cmux: native application evidence

cmux's [official documentation](https://cmux.com/docs/getting-started) describes
a native macOS terminal/browser, not a terminal UI that emits its display on
stdout. A meaningful native experiment should report **launch to owned-workspace
readiness**, command-to-observable output, and complete app/process-tree memory
with the window state and helper-process inclusion specified. These must remain
separate from PTY-byte echo and server-only RSS. Socket-command acknowledgement
is not a GPU presentation receipt.

Upstream documents [tagged development builds](https://github.com/manaflow-ai/cmux/blob/main/skills/cmux-dev-workflow/references/tagged-builds.md)
and an [isolated Release staging route](https://github.com/manaflow-ai/cmux/blob/v0.64.25/scripts/reloads.sh).
A separate bundle identity, socket, configuration/state directories, and a
non-user workspace are required. A temporary HOME alone is not sufficient for
macOS app preferences or all cmux state. Do not run a benchmark against a user's
existing cmux socket, relaunch their app, enable global accessibility settings,
or steal focus to manufacture a result. A dedicated macOS login/VM or an
explicitly available GUI test session is the appropriate boundary when startup
cannot be proven non-activating.

For the September 30 review, the official cmux `0.64.25` DMG was downloaded,
its SHA-256 checked against the release digest, and its `Info.plist` inspected
read-only: build `106`, revision `b685a275c`. It was not launched. The matching
[startup implementation](https://github.com/manaflow-ai/cmux/blob/v0.64.25/Sources/AppDelegate.swift)
defaults `shouldActivate` to true, then focuses the first window using
`activateAllWindows` with activation suppression disabled. The staging script's
`open -g` is therefore not sufficient evidence of a focus-safe launch.
The [retained feasibility record](https://docs.phux.sh/benchmarks/2026-09-30/cmux-feasibility.json)
contains the release provenance and exact prerequisite. No cmux numerical
ranking is implied by this missing GUI measurement.

## Interpreting a result

- One host is not a population study. Record power mode, concurrent load,
  versions, geometry, workload, and number of complete runs with the evidence.
- Local UDS, loopback QUIC, and shaped QUIC answer different questions.
- Low key-echo latency says nothing by itself about attach latency, resize,
  browser automation, agent detection, or session recovery.
- A slow or failed path belongs in the report. Do not erase it because a faster
  lane exists, or turn an unmeasured product into a last-place score.
- These comparisons are not CI budgets. Deterministic checks and regression
  coverage are described in [Quality bar](./architecture/verification.md#performance).
