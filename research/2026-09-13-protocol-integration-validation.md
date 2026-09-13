---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-13
---

# Protocol audit integration validation

**TL;DR.** The audit remediation is validated at transport, runtime, and client
boundaries. Counts below describe specific commands and source revisions, not a
full CI claim. Performance experiments remain diagnostic loaded-host results.

## Control dispatch and stream retirement

`5121c695` and `33c75146` add production runtime acceptance over UDS and real
QUIC. The QUIC case negotiates terminal streams, waits for a transcriber start
marker, holds transcription for at least two seconds, and completes PING,
GET_STATE, APPLY_INPUT and ROUTE_INPUT on the same connection within one second.
A separate real PTY verifies input order. PUT_FILE followed by transcription
checks FIFO and exact request correlation. UDS cases exercise connection/global
saturation, refusal without starting work, cancellation, and budget release.

Runtime unit tests additionally hold receipt capacity at zero before input
admission: a refused operation id can subsequently carry a different payload
without a dedupe conflict. Aborting a receipt blocked on the reply mailbox
releases its slot and cannot publish a delayed completion.

The upload worker's admission and cancellation are unit-tested. A deterministic
public-connection test holding the disk worker after admission requires a
testkit barrier and is tracked as `phux-sk2g`; the real FIFO test does not claim
to be a stalled-disk test.

Retirement signals a writer to drain admitted messages, waits at most one
second, then aborts and reaps. Concurrent retired writers are connection-owned
and capped at 127. A cancelled partial QUIC write resets the stream with `0x10`;
only a successful explicit close emits FIN. The real 32 KiB stream-credit
regression holds a write pending, confirms quiet-stream progress, and releases
that same write by draining the held stream. A cancelled mux read retains its
pending lifecycle event when the event channel is full.

The broader federation gate caught satellite ROUTE_INPUT being intercepted by
the local input lane. The fast path now accepts only local resource ids and
preserves resource-use bookkeeping; satellite commands use federation dispatch.

## Bounded diagnostics

`GET_PERF` includes `stream_diagnostics`; `phux perf --json` and JSON watch retain
the latest sample. A process-keyed connection hash and QUIC stream number
correlate samples with bind debug logs. Registry capacity is 128 streams, queue
metadata capacity is 256 items per stream, and excess registrations allocate no
tracker storage. Overflow is explicit.

Ingress tickets cover admitted incomplete bodies, queued frames, and blocked
channel sends until delivery or cancellation. They do not claim to measure
outbound mailbox backlog. Writer guards expose in-progress and completed write
durations, without attributing every delay to flow control. Bind-to-staged-READY
and admitted bootstrap tombstone reasons supply lifecycle context. Stale
tombstones cannot overwrite the current stream's reason.

Real QUIC tests verify queued byte accounting until delivery and active stalled
write age followed by reset/unregistration. Helper tests exercise cancellation,
out-of-order completion, overflow, reset, and registration reuse. Fresh review
found and corrected allocation before registry admission; suppressed trackers
are now allocation-free no-ops. The optional public `PerfReport` field is
JSON-compatible with older reports; this unpublished workspace crate's Rust
struct shape changes and its workspace constructors are updated together.

## Recorded validation

| Check | Recorded result |
|---|---|
| `cargo test --locked -p phux-tui --lib -- --test-threads=1` | 1,037 passed, one ignored |
| `cargo test --locked -p phux-client --test connection quic -- --test-threads=1` | 11 passed |
| `cargo test --locked -p phux-client --lib --test command_isolation -- --test-threads=1` | 340 library and one real-QUIC test passed |
| `cargo test --locked -p phux-perf` | 21 passed, including legacy JSON and watch preservation |
| `cargo test --locked -p phux-server --lib -- --test-threads=1` after native integration `83d433aa` | 1,139 passed |
| Final server library plus complete attach target after `eac205c1` fixes | 1,139 + 47 passed, including the real C ABI and eight-owner/50k release gate |
| Server protocol / federation / metadata / transport integration | 41 protocol, 22 hub/relay federation, 14 metadata, one connector, one relay e2e, 65 terminal passed; one terminal test ignored |
| Client core / FFI / protocol / relay combined gate | 271 core + two doctests; 185 FFI; 89 protocol + one golden + 13 allocation-abuse + 159 wire-contract + one doctest; 35 relay + 20 contract tests passed |
| Eight-package all-target/all-feature Clippy with `-D warnings` | Passed; final native fixture edits also passed the affected-package gate |
| Workspace rustdoc with `RUSTDOCFLAGS='-D warnings'`, all features, no dependencies | Passed |
| Server `--no-default-features` check | Passed, with 14 unused-code/import warnings in native-disabled configuration |
| Build feature boundaries, Zig pins, toolchain synchronization | Full/lean/MCP boundaries passed; Zig 0.16.0 and Rust 1.98.1 synchronized |
| Full real Chrome lane | Three required live-server/render tests and authenticated fallback passed; fallback 4.10 seconds with six blackholed WT datagrams |
| WASM session tests under Node | 12 passed |
| `bash scripts/doctor.sh native` | Zero problems |
| `bash scripts/check-docs.sh` | 207 files, zero violations |
| Python shaper and browser-harness unit tests | Nine and 15 passed, respectively |

The formerly intermittent foreground-flush fixture now waits in the shell
builtin `read`, rather than adding external-child exit scheduling to the
production 500 ms shutdown grace (`666327c1`). Product timing is unchanged.

## Native runtime acceptance and review

`83d433aa` drives real HELLO/ATTACH over a Unix socket into the public C ABI
kernel using the published dependency pin. It verifies official V1 negotiation,
zero-history FINISH and 3,000-row multipage history with a one-row local
retention cap. A held kernel request permits a real PTY marker between pages;
the resumed flow must authenticate FINISH and remain attached. Client tests
separately prove TooSmall growth survives Busy, with effect buffers cleared
between responses so earlier requests cannot satisfy later assertions.

Fresh review caught parked requests surviving resize/reattach. Both invalidation
paths now answer/remove those requests immediately, before bootstrap retirement;
the common writer fences responses that reach its queue after a tombstone.
Expiry answers parked requests with the same `Expired` reason as the control
notification. Tests assert completion without a later actor turn.

The FFI clear-presentation fixture now names official V1 and sends distinct
PAGE/FINISH units. It verifies delayed pages do not revive cleared presentation,
while a replacement generation imports history and finishes normally. Existing
server fixtures likewise request official V1. Eight simultaneous native owners
must independently finish; identical prefixes do not require identical opaque
cursor tokens under detached capture.

That eight-owner/50,000-row runtime gate also caught a double-promotion bug:
FINISH released its owner and promoted the backlog during cleanup, then the
outer step promoted again and dropped the first waiting request. Promotion now
preserves an already-pending request. The unchanged simultaneous-reader and
stalled-reader assertions pass after this repair; fresh review checked every
completion/error/cancellation path through promotion.

## Complexity evidence

The shared complexity skill was unavailable; `uvx --from lizard lizard` supplied
the measurements. Lizard's Rust parser is a comparative indicator, not a proof
that macro/`let else` control flow has no branches.

| Function | Before | After retirement/diagnostic wiring |
|---|---:|---:|
| `requires_terminal_stream` | 1 | 1 |
| `StreamBinding::retire` | 1 | 3 |
| `bind_stream` | 2 | 2 |
| `drop_stream_binding` | 1 | 2 |
| `drop_all_stream_bindings` | 1 | 2 |
| `close_for_cancellation` | 2 | 3 |
| mux `read_frame` | 7 | 8 |
| `pump_terminal_stream` | 6 | 6 |
| `read_framed_bounded` | 15 | 15 |

New helpers: `begin_retirement` 2, `retire_stream` 3, writer `with_lane` 1,
diagnostic `insert` 1, `enqueue_at` 2, and `begin_write_at` 2. The added lifecycle
branches implement bounded drain, cancellation and cleanup rather than hiding
them in dense expressions.

Final additions: tombstone-aware fence `admits` 2 → 5, `run_inner` 6 → 7,
`invalidate_all_native_cursors` 4 → 6 and `invalidate_native_owner` 5 → 5.
`record_tombstone` is 2; fixture `native_capture` is 4. Native constructor
`build` decreased 16 → 14 through behavior-preserving PTY initialization
extraction (new helper 4). Guarded `start_next_native_history` increased 1 → 2
to preserve a request already promoted by owner cleanup.

## Performance evidence

The [production-path report](2026-09-13-protocol-path-measurements.md) records
direct/raw, StateSync ACKs, explicit relay single-stream fallback, selective
unpolled streams, upload framing and active disconnect experiments. The
[wire encoding report](2026-09-13-wire-encoding-measurements.md) records actual
allocation savings. The [polling report](2026-09-13-polling-reuse-measurements.md)
records lower connection churn and CPU, without claiming a consistent detection
latency improvement. The 8 MiB upload case is the legal wire ceiling; no shipping
producer selects it, so it does not establish a new client upload policy.
