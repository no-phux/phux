---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-09-13
---
# Protocol production-path measurements

**TL;DR.** `phux-client` now has an ignored, serial measurement harness that
runs a real `ServerRuntime`, real PTYs, the production QUIC `Connection`, and
the bounded userspace UDP shaper. The default smoke is one selectable matrix
case, not a WAN claim. This report records only cases actually run; the full
acceptance matrix remains an explicit command until runtime APIs expose the
remaining per-stream queue and stalled-reader controls.

## What the harness measures

`crates/phux-client/tests/protocol_path.rs` creates one isolated server and one
QUIC connection per case. A case with 1, 8, or 32 Terminals binds every
Terminal on that single connection; it never creates one connection or relay
per Terminal. Raw and StateSync modes are separate matrix dimensions.
The QUIC UDP socket is reached through `scripts/bench/udp-delay.py`, which adds
the selected full-duplex delay, seeded loss, and UDP-payload serialization
rate. Any queue drop, shutdown drop, malformed metrics file, or nonzero shaper
exit invalidates the test.

The quiet PTY first runs `stty -echo` and then transforms each input nonce into
`PHUX_RESPONSE_<nonce>`. Echo latency ends only when that exact transformed
byte sequence returns in that Terminal's production `RESOURCE_OUTPUT`. Nonces
are typed as key events and submitted with Enter, avoiding bracketed-paste
markers in the probe program; shell echo and arbitrary flood output cannot
satisfy the sample. Each other PTY reports a correlated start marker, waits at
a barrier, and is then told to generate a bounded requested flood while
quiet-pane echo and control `PING`/`PONG` samples run. `requested_bytes` is the
exact formatted line volume requested from the PTYs; it is not a claim that all
bytes reached the client before the timed samples ended. Readiness and start
tokens are assembled by shell `printf` substitutions, so the full token is
absent from the submitted command and cannot match the shell's command echo.

Reported fields mean:

- `ready_us`: ATTACH send to each Terminal's `BOOTSTRAP_READY` for the
  `BOOTSTRAP_BEGIN` generation observed after the actual stream bindings.
- `echo_us`: first quiet-pane key send to its correlated transformed response.
- `control_us`: control-stream PING send to matching PONG.
- `bootstrap_*` and `output_*`: frames and useful payload bytes observed by the
  production client, not UDP/IP bytes.
- `shaper_metrics`: UDP payload packets/bytes, random drops, queue drops, peak
  queue depth, and oldest queue age in each direction.
- `server_perf`: the actual production `GET_PERF` report. It remains
  process-wide where the runtime has no bounded per-stream labels.
- `cancellation_us`: shutdown signal to joined `ServerRuntime` task. The
  shaper is independently stopped with `SIGTERM` and waited before teardown.

Latency summaries use documented nearest-rank percentiles
(`rank = ceil(p * N)`). READY observations within one row are Terminals from a
single attach, not independent benchmark runs; p95/p99 are descriptive only.

Raw/native output sends no `FRAME_ACK`. StateSync cases explicitly negotiate
`SynthesizedVtStateSync`, cumulatively ACK each observed output sequence, and
fail unless at least one real ACK was sent. Component allocation counters are
not substituted for wire or CPU results.

## Commands

### Integrated representative rerun

The integrated runtime at `9dd16ec3`, using the public `d2fd87aa` libghostty
pin subsequently committed as `f24bc246`, passed the following serial
representatives. These are smoke observations with two echo/control samples,
not statistically significant latency comparisons. The eight-terminal rows
use the default unshaped-delay representative; upload rows use 50 ms RTT and
10 Mbit/s full-duplex UDP-payload budgets.

| Route/mode | READY p50/max | Echo p50/max | Control p50/max | Runtime shutdown |
|---|---:|---:|---:|---:|
| Direct/raw | 7.978/8.704 ms | 4.677/13.252 ms | 24.486/47.787 ms | 2.036 ms |
| Direct/StateSync | 8.906/9.649 ms | 13.028/19.711 ms | 1.796/2.106 ms | 3.971 ms |
| Relay/raw, single-stream fallback | 5.229/7.704 ms | 5.073/23.571 ms | 56.761/77.560 ms | 3.123 ms |

StateSync sent 20 real ACKs for 28,337 output bytes. Direct/raw observed
1,175,804 output bytes; relay/raw observed 1,213,412 and explicitly negotiated
`multistream:false`. Each requested 1,835,337 flood bytes. The selective-unpolled
representative requested 16,777,256 bytes after the marker barrier, received
quiet echo in 0.973 ms and control in 99.729 ms, and shut down in 2.871 ms.
Every case reported zero random, queue, and shutdown drops in both directions.

The 8 MiB upload took 36.961753 s with 16 KiB chunks (1.815629 Mbit/s), versus
7.048326 s for the one-frame legal wire-ceiling experiment (9.521248 Mbit/s).
The wire-ceiling send itself blocked 5.997715 s; concurrent echo/control took
897.257/1,024.179 ms. All upload shaper drop counts were zero. This repeats the
earlier goodput/latency tradeoff; it does not change shipping producer policy.

The final active-disconnect case recompiled after the checkout reached
`96bee054` with pending integration repairs, so it is recorded separately:
the 8 MiB send stayed pending after 500 ms at 0.3 Mbit/s, only 57,045 upstream
bytes crossed, and server connection cleanup completed in 1.046443 s. The
test's shaper validation passed. Complete logs are retained in the session
artifact directory `protocol-path-9dd16ec3/` (direct-raw, direct-state-sync,
relay-raw, selective-unpolled, put-file-16k-8m-10mbit-50ms, and
active-upload-disconnect logs).

### Reproduction

Run the bounded default case serially, after other builds are quiet:

```sh
CARGO_BUILD_JOBS=1 cargo test --locked -p phux-client \
  --test protocol_path shaped_quic_protocol_path_matrix \
  -- --ignored --nocapture --test-threads=1
```

Run the full routed-QUIC shape matrix (432 cases):

```sh
PHUX_PROTOCOL_PATH_ROUTES=direct,relay \
PHUX_PROTOCOL_PATH_MODES=raw,state-sync \
PHUX_PROTOCOL_PATH_TERMINALS=1,8,32 \
PHUX_PROTOCOL_PATH_RTT_MS=0,50,150,300 \
PHUX_PROTOCOL_PATH_LOSS_PERCENT=0,1,3 \
PHUX_PROTOCOL_PATH_MBIT=0.3,3,30 \
CARGO_BUILD_JOBS=1 cargo test --locked -p phux-client \
  --test protocol_path shaped_quic_protocol_path_matrix \
  -- --ignored --nocapture --test-threads=1
```

`PHUX_PROTOCOL_PATH_MODES` accepts `raw` and `state-sync`.
`PHUX_PROTOCOL_PATH_SAMPLES` changes the echo/control sample count and
`PHUX_PROTOCOL_PATH_FLOOD_BYTES` changes the bounded payload generated by each
non-quiet PTY. Allowed topology/path values are deliberately restricted to the
audit matrix so a typo cannot silently produce incomparable evidence.

Run the selective stalled-reader regression separately:

```sh
CARGO_BUILD_JOBS=1 cargo test --locked -p phux-client \
  --test protocol_path stalled_terminal_reader_preserves_quiet_echo_and_control \
  -- --ignored --nocapture
```

That case creates eight real Terminals and binds a quiet and a flood Terminal
stream with the production QUIC dialer and server. It waits for a correlated
flood-start marker, releases a bounded 16 MiB requested flood, and then retains
that flood `RecvStream` without polling it while continuing to drain the quiet
Terminal stream and control stream. The test requires a correlated quiet
response and matching PONG before their explicit deadlines. It proves useful
work continues while one real Terminal receive stream is unpolled; without
stream-credit instrumentation it does **not** claim the receive window became
blocked. This low-level read-control case does not claim to exercise the
production `Connection` receive merger; the ordinary matrix does.

The complementary transport regression
`window::tests::exhausted_stream_credit_preserves_other_stream_progress` in
`phux-dial` proves actual stream-credit exhaustion with production
`TrackedSend`/`SendWindow`. Its peer advertises exactly 32 KiB per stream and
512 KiB connection credit. After filling the first stream's 32 KiB, its next
one-byte write remains pending for 100 ms. A second stream sharing the same
connection window delivers `quiet`, while the original write remains pending
for another 100 ms. Draining the first receiver then releases that exact
pending write and delivers its final byte. The whole test has a five-second
deadline and owns all endpoint/stream handles. This isolates flow-control
credit from aggregate UDP traffic; it is a transport-level complement to the
production Terminal experiment, not a claim about that experiment's queue.

```sh
CARGO_BUILD_JOBS=1 cargo test --locked -p phux-dial --lib \
  window::tests::exhausted_stream_credit_preserves_other_stream_progress \
  -- --exact --nocapture
```

Parent validation: passed in 0.21 seconds; strict all-target/all-feature
`phux-dial` Clippy passed.

Run the production `PUT_FILE` chunk experiment at 50 ms RTT separately. The
10 Mbit/s cases send an 8 MiB file, so the 8 MiB case emits the largest legal
production wire command. Repository search found no shipping upload producer
that selects this size: it is a wire-ceiling stress experiment, not a shipping
client policy. The 0.3 and 3 Mbit/s thin-path cases send 256 KiB to keep the
experiment bounded; their `actual_max_chunk_bytes` field therefore says
256 KiB even when the configured experiment chunk is 8 MiB.

```sh
PHUX_PROTOCOL_UPLOAD_CHUNK_KIB=16,64,256,8192 \
PHUX_PROTOCOL_UPLOAD_MBIT=10 \
CARGO_BUILD_JOBS=1 cargo test --locked -p phux-client \
  --test protocol_path put_file_chunk_matrix \
  -- --ignored --nocapture --test-threads=1

PHUX_PROTOCOL_UPLOAD_CHUNK_KIB=16,64,256,8192 \
PHUX_PROTOCOL_UPLOAD_MBIT=0.3,3 \
CARGO_BUILD_JOBS=1 cargo test --locked -p phux-client \
  --test protocol_path put_file_chunk_matrix \
  -- --ignored --nocapture --test-threads=1
```

Each chunk is sent through `Connection::send` and acknowledged by its real
`COMMAND_RESULT`. Immediately after that call returns, the harness sends a
correlated PING on the control stream and types a correlated nonce into a
second bound Terminal. `send_us` records time spent inside the production send
call; `echo_us` and `control_us` start after it returns. Thus the 8 MiB result
does not disguise sender blockage as Terminal-stream isolation. Upload time
runs from the first send attempt through the final acknowledged publication;
goodput uses useful file bits, not UDP/IP bytes. The published file is read
back and compared byte-for-byte. RSS-after is sampled after that verification,
payload drop, and a 100 ms settling interval while server and connection remain
alive. The old `cancellation_us` field was only orderly `ServerRuntime::stop`
after a completed upload; it is now correctly named `server_shutdown_us` and
is not presented as active-upload cancellation evidence.

Run the active disconnect case separately:

```sh
CARGO_BUILD_JOBS=1 cargo test --locked -p phux-client \
  --test protocol_path inflight_upload_disconnect_releases_server_connection \
  -- --ignored --nocapture --test-threads=1
```

This sends one legal 8 MiB wire-ceiling frame over a 0.3 Mbit/s shaped path,
requires the production `Connection::send` to remain backpressured, and then
aborts the task that owns the connection. A fresh production connection polls
`GET_PERF` until `proc.clients` proves the aborted connection was removed. The
shaper must show a meaningful but incomplete upstream prefix with zero queue or
shutdown drops. No production metric currently exposes upload-worker budget or
writer retention, so this case proves prompt connection teardown only; budget
release under an already admitted worker remains pending instrumentation.

The bounded run passed: `Connection::send` was still backpressured at 500 ms,
the task owning it was aborted, and the shaper recorded 56,942 total upstream
bytes while the complete 8 MiB frame had not crossed. `proc.clients` showed
cleanup after 1.046 s. Queue and shutdown drops were zero in both directions.
The final server stop was only test fixture cleanup and is not reported as
upload cancellation.

## Results actually run

The original rows below are retained as **provisional historical diagnostics**:
they predate generation-qualified READY matching, nearest-rank percentiles,
correlated flood-start barriers, and hard rejection of nonzero shaper queue or
shutdown drops. They must not be used as accepted timing evidence.

These are bounded correctness-smoke results from the active development host,
not quiet-host performance evidence and not WAN measurements. They demonstrate
that the cases complete without converting a timeout, protocol refusal, shaper
overflow/nonzero exit, missing transformed nonce, or incomplete READY set into
a sample.

| Route/mode | Terminals | Shape | Samples/flood | READY p50/max | Echo p50/max | Control p50/max | ACKs | Result |
|---|---:|---|---|---:|---:|---:|---:|---|
| direct/raw | 1 | 0 ms, 0%, 30 Mbit/s | 1 / 256 KiB | 3.903/3.903 ms | 21.551/21.551 ms | 0.588/0.588 ms | 0 | pass |
| direct/raw | 8 | 0 ms, 0%, 30 Mbit/s | 2 / 256 KiB | 17.757/18.412 ms | 46.737/147.259 ms | 117.440/147.772 ms | 0 | pass |
| direct/raw | 32 | 0 ms, 0%, 30 Mbit/s | 1 / 64 KiB | 20.280/27.332 ms | 120.893/120.893 ms | 14.194/14.194 ms | 0 | pass |
| direct/raw | 1 | 50 ms, 1%, 3 Mbit/s | 1 / 256 KiB | 117.715/117.715 ms | 54.796/54.796 ms | 53.027/53.027 ms | 0 | pass |
| direct/raw | 1 | 300 ms, 3%, 0.3 Mbit/s | 1 / 256 KiB | 880.485/880.485 ms | 326.610/326.610 ms | 304.253/304.253 ms | 0 | pass |
| direct/StateSync | 1 | 0 ms, 0%, 30 Mbit/s | 1 / 256 KiB | 2.788/2.788 ms | 21.913/21.913 ms | 0.459/0.459 ms | 2 | pass |
| direct/StateSync | 8 | 0 ms, 0%, 30 Mbit/s | 1 / 64 KiB | 8.821/9.521 ms | 22.948/22.948 ms | 3.128/3.128 ms | 9 | pass |
| relay/raw | 8 | 0 ms, 0%, 30 Mbit/s | 1 / 256 KiB | 7.566/8.093 ms | 24.746/24.746 ms | 20.704/20.704 ms | 0 | pass |
| direct/raw selective stall | 8 | 0 ms, 0%, 30 Mbit/s | 16 MiB requested | n/a | 0.760 ms | 106.564 ms | 0 | pass |

All runs also returned parseable production `GET_PERF` and zero shaper queue,
shutdown, and random drops except that the seeded 1% smoke happened to draw no
drop in its 65 packets. The old selective-stall shaper forwarded 1.332 MiB
before shutdown. That aggregate UDP count does not establish which stream
consumed the bytes or that its receive window blocked.

### Post-review representatives

The two samples in echo/control rows use nearest-rank tails; READY samples are
the eight Terminals in one attach. Each matrix case requested 1,835,337 flood
bytes from seven PTYs. The direct/raw and selective rows were rerun after making
the complete marker absent from the shell command. The StateSync and relay rows
predate that correction and remain provisional; they are retained only to show
the negotiation/ACK checks that passed.

| Route/mode | Terminal streams | READY p50/max | Echo p50/max | Control p50/max | ACKs | Result |
|---|---|---:|---:|---:|---:|---|
| direct/raw | negotiated | 7.884/8.649 ms | 2.913/13.312 ms | 23.060/44.400 ms | 0 | pass |
| direct/StateSync | negotiated | 9.251/9.932 ms | 2.615/21.264 ms | 1.677/1.827 ms | 18 | provisional: pre-marker fix |
| relay/raw | explicit single-stream fallback | 9.943/12.748 ms | 3.131/14.058 ms | 46.010/87.934 ms | 0 | provisional: pre-marker fix |
| direct/raw selective unpolled stream | negotiated | n/a | 0.952 ms | 96.106 ms | 0 | pass |

All four runs hard-rejected queue/shutdown drops and completed with zero of
either. Only the direct/raw and selective rows are marker-safe accepted timing
evidence. The selective case recorded its non-echoable flood-start marker before
leaving the stream unpolled and requested 16,777,256 bytes; it does not claim
that volume was delivered or that stream flow control was exhausted.

### PUT_FILE diagnostic matrix

These original measurements are **provisional historical diagnostics** because
their p95/p99 calculation predates nearest-rank selection and their shaper
validation did not hard-reject nonzero queue/shutdown counters. They came from
the same active development host and carry the same loaded-host-only
qualification. Latency cells are p50/p95/p99/max in ms.
The 16, 64, and 256 KiB rows are harness proposals only; no chunk selection is
recommended here. The 8 MiB/10 Mbit/s row is the legal wire-ceiling experiment,
not a shipping producer policy. Every row completed file verification,
parseable `GET_PERF`, clean shaper shutdown with zero drops, and bounded orderly
server shutdown.

| Experiment chunk | File | Rate | Total | Goodput | Send | Chunk ACK | Quiet echo | Control | RSS delta | Server shutdown |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 16 KiB | 8 MiB | 10 | 35.743 s | 1.878 Mbit/s | 0.010/0.013/0.023/0.052 | 65.176/80.151/85.022/137.253 | 53.539/55.137/59.267/69.725 | 65.167/80.144/85.011/137.214 | +16.88 MiB | 0.634 ms |
| 64 KiB | 8 MiB | 10 | 13.652 s | 4.916 Mbit/s | 0.016/0.042/0.072/1.206 | 105.336/109.533/122.988/195.525 | 53.399/54.033/55.256/98.219 | 105.311/109.517/122.934/195.471 | +0.67 MiB | 1.191 ms |
| 256 KiB | 8 MiB | 10 | 8.635 s | 7.771 Mbit/s | 0.030/0.073/0.085/0.175 | 266.232/266.844/279.211/367.947 | 53.584/54.563/58.167/59.086 | 266.205/266.819/279.140/367.863 | +0.89 MiB | 0.987 ms |
| 8 MiB | 8 MiB | 10 | 7.055 s | 9.512 Mbit/s | 6069.182/6069.182/6069.182/6069.182 | 7054.264/7054.264/7054.264/7054.264 | 896.983/896.983/896.983/896.983 | 985.084/985.084/985.084/985.084 | +17.11 MiB | 1.283 ms |

The thin-path file is 256 KiB, so the 8 MiB experiment rows below emit one
256 KiB chunk and intentionally duplicate the 256 KiB wire shape. Thin-path
latency cells are p50/max in ms; full percentiles remain in the machine output.

| Experiment chunk | Actual max | Rate | Total | Goodput | Quiet echo | Control | RSS delta | Server shutdown |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 16 KiB | 16 KiB | 0.3 | 8.353 s | 0.251 Mbit/s | 133.180/136.652 | 520.968/531.375 | +0.83 MiB | 0.671 ms |
| 16 KiB | 16 KiB | 3 | 1.634 s | 1.283 Mbit/s | 59.681/60.378 | 98.426/133.823 | +0.19 MiB | 0.568 ms |
| 64 KiB | 64 KiB | 0.3 | 7.463 s | 0.281 Mbit/s | 132.401/134.410 | 1861.674/1871.105 | +0.48 MiB | 1.269 ms |
| 64 KiB | 64 KiB | 3 | 0.997 s | 2.103 Mbit/s | 62.006/68.687 | 236.731/278.648 | +0.25 MiB | 1.496 ms |
| 256 KiB | 256 KiB | 0.3 | 7.258 s | 0.289 Mbit/s | 134.349/134.349 | 7258.295/7258.295 | +0.88 MiB | 1.728 ms |
| 256 KiB | 256 KiB | 3 | 0.807 s | 2.598 Mbit/s | 59.208/59.208 | 807.262/807.262 | +1.05 MiB | 0.631 ms |
| 8 MiB | 256 KiB | 0.3 | 7.246 s | 0.289 Mbit/s | 140.649/140.649 | 7246.226/7246.226 | +0.23 MiB | 1.932 ms |
| 8 MiB | 256 KiB | 3 | 0.809 s | 2.592 Mbit/s | 60.443/60.443 | 808.841/808.841 | +0.02 MiB | 1.090 ms |

The measured facts are intentionally left for the parent chunk-selection
design. At 10 Mbit/s, useful goodput was 1.878, 4.916, 7.771, and 9.512 Mbit/s
for 16 KiB, 64 KiB, 256 KiB, and 8 MiB respectively. The 8 MiB case held
`Connection::send` for 6.069 s and its process RSS delta was +17.11 MiB. After
that send returned, the second-Terminal echo took 896.983 ms and the control
PING took 985.084 ms.

The required refreshed legal wire-ceiling representative passed after the
review fixes: 8 MiB at 10 Mbit/s completed in 7.048 s at 9.522 Mbit/s goodput;
`Connection::send` occupied 6.054 s, then quiet echo took 892.237 ms and PING
took 993.333 ms. RSS delta was +35.13 MiB and orderly server shutdown was
0.490 ms. The shaper reported zero random, queue, and shutdown drops in both
directions.

## Deliberate limits

The relay case runs the production `RelayRuntime`, production server connector,
and production client `Connection` with disposable route, tunnel, and consumer
credentials. Bilateral QUIC negotiation now enables Terminal streams only on
the direct route; the relay advertises explicit single-stream fallback and the
harness requires `multistream:false`. Direct cases require `multistream:true`.
Federation, WebSocket, and WebTransport are separate route cases rather than
aliases for loopback QUIC.

The current public `Connection` merges bound-stream reads into one queue, so
selective read control uses the production dialer and wire protocol directly,
as documented above, rather than pretending a whole-client stall is
per-Terminal. Queue/credit/cwnd and resync reason are recorded only when the
integrated QUIC runtime exposes real bounded diagnostics. Those missing values
remain acceptance work, not zeroes.
