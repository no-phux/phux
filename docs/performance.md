---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-09-16
---

# Performance

**TL;DR.** A dated local comparison of phux and Herdr using the repository's
reproducible multiplexer benchmark. On this host, phux had lower median key-echo
latency and similar server memory after bulk output; Herdr reattached faster in
the loaded-history case. These measurements are evidence, not universal product
rankings.

## Result

These results were measured on September 2, 2026. The run used:

- phux `0.23.3` at commit `c90e2f33cfece36c9566d6949a3ef15d8f5b078f`;
- Herdr `0.8.2`;
- an Apple M4 Pro with 48 GiB RAM, macOS 27.0, a 120x40 terminal;
- isolated release servers with separate `HOME`, XDG directories, and sockets;
- 300,000 output lines for bulk output and four 188x40 panes with 60,000 lines
  each for the loaded-history case.

UDS means the local Unix-domain socket transport. QUIC and WebSocket results in
this run also used loopback rather than a shaped network path.

| Measurement | phux | Herdr |
|---|---:|---:|
| Local PTY key echo, p50 | 176 µs (UDS) | 791 µs |
| Server RSS after 300,000 lines | 28.7 MB (UDS) | 33.3 MB |
| 300,000 lines, marker painted | 375 ms (WebSocket) | not recorded |

Attach timings isolate different parts of startup rather than forcing unlike
measurements into one row:

| phux attach measurement | Result |
|---|---:|
| Cold attach over UDS | 64 ms |
| Cold attach over QUIC | 60 ms |
| Warm attach over UDS | 70 ms |
| UDS connection to `ATTACHED` protocol response | 1.07 ms |

The history-loaded scenario is the less flattering and more useful case:

| Measurement, four panes x 60,000 lines | phux | Herdr |
|---|---:|---:|
| Server RSS while a client is attached | 46 MB | not recorded |
| Warm reattach median | 117 ms | 85 ms |
| PTY key echo p50 | 174 µs | 12,447 µs |

The original raw run directory was not retained. The values above are the
surviving campaign record; the command below produces a new timestamped raw
result directory rather than reproducing these exact samples byte for byte.

## What is being timed

The benchmark owns a pseudoterminal and drives each multiplexer through it. For
key echo, the clock starts when a byte is written to that PTY and stops when the
echo returns. This avoids the approximately millisecond-scale polling floor of
screen capture and measures the path a person feels.

Bulk output starts `seq`, waits until a unique marker is painted by the client,
and records wall time plus server and client CPU deltas. The memory row samples
the server after that run. The loaded-history case creates four panes, fills
each beyond the retained-history limit, repeatedly attaches, and introduces a
second client during the echo probe.

The tmux lane is the private terminal that hosts every visual measurement. It
provides the benchmark's PTY and capture mechanism; this result set does not
claim equivalent tmux throughput, attach, or server-memory numbers.

## Reproduce it

Build phux in release mode, install Herdr and tmux, then run:

```sh
cargo build --locked --release -p phux
scripts/bench/mux-compare.sh --mux all --big-history \
  --phux-bin target/release/phux \
  --herdr-bin /opt/homebrew/bin/herdr
```

The script prints the result table and writes raw samples, server logs, probe
JSON, and the exact commands under `target/bench/mux-compare-<timestamp>/`. It
also supports `--rtt-ms`, `--path-mbit`, and `--loss-percent` for a shaped QUIC
path. Run benchmarks on an otherwise idle host; compiler load has materially
changed tail latency in prior investigations.

## Limits

- This is one Apple-silicon host, not a population study. OS scheduling,
  terminal geometry, shell output, and installed versions affect the result.
- The raw September 2 sample directory is no longer available. The retained
  campaign record supports the values above, but not recalculation or alternate
  percentiles from that run.
- The phux lanes share one binary but exercise different transports. The local
  UDS result should not be presented as remote-network performance.
- Herdr and phux retain and render different product models. Equal fixture
  input makes the comparison reproducible; it does not make every unit of work
  architecturally identical.
- These numbers are published evidence, not CI budgets. The deterministic
  performance checks and current regression-gate gap live in
  [Quality bar](./architecture/verification.md#performance).
- Feature choice should start with [When to use phux](./when-to-use.md), not the
  smallest number in this table.
