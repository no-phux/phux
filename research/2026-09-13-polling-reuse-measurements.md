---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-09-13
---
# Polling connection-reuse measurements

**TL;DR.** A serial ignored experiment compares the unchanged one-shot screen
helper with production persistent waits over Unix sockets against a real
`ServerRuntime` and PTY-backed Terminal. It measures accepted connections,
process rusage CPU, and marker-detection latency for 1, 8, and 32 concurrent
agents on idle and changing screens. Results below are loaded-host diagnostics.

## Method

`crates/phux-client/tests/polling_reuse.rs` runs each case in an isolated
current-thread runtime. The server is the production `ServerRuntime`, seeded
with the host's default shell in a real PTY. Poll requests use the production
Unix-domain-socket framing and handshake.

A transparent Unix-socket proxy sits between the clients and server solely to
count accepted client connections. Each accepted frontend socket maps
one-for-one to a backend server socket and is pumped byte-for-byte with
`copy_bidirectional`; it does not decode or synthesize protocol traffic. The
warm-up read and changing-screen input use the backend socket directly, outside
the counter, so the reported count covers polling agents only.

The old path calls the still-public `get_screen_scrollback` helper once per
poll, which opens and negotiates a new connection. The persistent path calls
the public `poll_until` API, which owns one `ScreenPollConnection` per bounded
wait and reconnects only on transport loss. Idle cases wait 750 ms for an
absent marker. Changing cases route `printf MARKER` to the PTY after 400 ms and
wait up to 3 seconds. Both use a 25 ms requested interval.

Process CPU is the actual `getrusage(RUSAGE_SELF)` delta exposed by
`phux_perf::ProcessStats`. Because client, proxy, and in-process server share a
process, it is an honest whole-experiment CPU figure rather than a fabricated
client-only attribution. It is also sensitive to unrelated host load, so the
experiment prints diagnostics and does not impose a CI CPU threshold.

## Run it

Run serially with a private target directory while other builds are quiet:

```sh
CARGO_TARGET_DIR="$TMPDIR/phux-polling-reuse-target" \
CARGO_BUILD_JOBS=1 cargo test --locked -p phux-client \
  --test polling_reuse polling_reuse_measurement_matrix \
  -- --ignored --nocapture --test-threads=1
```

The output is one JSON object per case. `connections` is observed at the proxy,
`cpu_us` is process CPU consumed during the case, and `max_detection_us` is the
slowest agent's wall time (the timeout duration for idle cases).

## Results actually run

Results are recorded after running the command above on the active development
host. They are loaded-host diagnostics, not WAN or quiet-lab claims.

| Workload | Agents | Path | Polls | Connections | CPU | Max detection |
|---|---:|---|---:|---:|---:|---:|
| idle | 1 | one-shot | 28 | 28 | 11.793 ms | 751.405 ms |
| idle | 1 | persistent | 27 | 1 | 6.358 ms | 750.699 ms |
| idle | 8 | one-shot | 216 | 216 | 43.825 ms | 753.571 ms |
| idle | 8 | persistent | 216 | 8 | 20.814 ms | 751.959 ms |
| idle | 32 | one-shot | 800 | 800 | 131.296 ms | 756.156 ms |
| idle | 32 | persistent | 800 | 32 | 67.300 ms | 750.472 ms |
| changing | 1 | one-shot | 16 | 16 | 8.005 ms | 418.824 ms |
| changing | 1 | persistent | 16 | 1 | 4.216 ms | 410.417 ms |
| changing | 8 | one-shot | 128 | 128 | 23.677 ms | 433.528 ms |
| changing | 8 | persistent | 128 | 8 | 12.670 ms | 419.036 ms |
| changing | 32 | one-shot | 448 | 448 | 71.761 ms | 420.993 ms |
| changing | 32 | persistent | 480 | 32 | 37.884 ms | 417.620 ms |

Across these six paired cases, reuse reduced observed polling connections by
92.9% to 96.4% and process CPU by 46.1% to 52.5%. Changing-screen detection was
8.407 ms faster for one agent, 14.492 ms faster for eight, and 3.373 ms faster
for 32. The 32-agent persistent case completed one extra polling round per
agent before observing the marker (480 versus 448 total polls), yet still used
47.2% less process CPU. These are results from one loaded-host run, not stable
latency baselines.

## Acceptance evidence

The non-ignored `persistent_wait_reconnects_once_and_retains_its_deadline` test
uses the same real runtime and counted proxy. The proxy drops the first
connection between polls; the wait must reconnect exactly once, complete the
multi-read idle condition, and return inside its original two-second deadline.
This pins bounded recovery independently of the comparative measurements.

The experiment asserts the connection invariant for every case: the one-shot
path opens exactly one connection per completed poll, while the persistent path
opens exactly one connection per concurrent wait. CPU and latency remain
reported measurements rather than flaky performance assertions.
