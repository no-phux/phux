---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-10-07
---

# Operations

**TL;DR.** `phux status` answers whether the server is up. Logs land
owner-only under the state directory; `phux logs` finds them. A panic
aborts the whole daemon, not one pane. Anyone with the same UID can
connect to the Unix socket; a remote bearer token is command-execution
access. There is no access audit log.

---

## Error model

Library and binary boundaries use typed Rust errors appropriate to their
module; there is no single workspace-wide error enum. Errors that cross
the IPC boundary translate to `ERROR` messages with a stable `ErrorCode`
and a human-readable message. [`spec/proto.md`](./spec/proto.md) owns that
wire shape and the code catalog.

A CLI stdout `BrokenPipe` (`phux snapshot work | head -8`) exits silently
with status `0`. Other write failures print one stderr line and exit non-zero.
Every stdout write uses a shared helper; the bin crate's `print_stdout`
lint prevents `println!` from bypassing it.

A **malformed `config.toml` is fatal at server start**: the server refuses to
start and reports the config path, the loader error, and the remedy
(`phux config check`) on both stderr and the server log (the auto-spawn path
nulls stdio). A *missing* config starts with the shipped defaults. The server
never silently reverts a broken config to defaults.

## Runtime status

`phux status` reports the listening server's PID (from UDS peer credentials),
socket bind time (preserved across graceful upgrade), negotiated protocol
version, total attached clients, sessions, satellite-terminal counts, and log
paths. It uses existing wire operations, including a real
`HELLO`/`HELLO_OK` exchange. Partial federation results are identified.

`--json` emits a stable versioned document. With no server, human output
shows the standard diagnostic and JSON output is `{"running": false, ...}`;
both exit 1.

Per-pane scrollback is a config knob, not an ops table: raising
`history-limit` on a wide grid does nothing, and raising `history-bytes`
costs resident memory per pane, not attach latency. See
[`CONFIG.md`](./CONFIG.md#scrollback).

## Logging and observability

Logging uses `tracing`, initialized in `phux_server::telemetry`; server and
TUI entry points share a layer builder
([ADR-0028](adr/0028-runtime-log-control.md)).

- **Server / foreground** (`phux server`, one-shot control verbs, any
  `--json` path) — `telemetry::init()`. Always logs human-or-JSON text to
  **stderr**; stdout is reserved for protocol/PTY traffic.
- **Client / TUI** (`phux attach`, naked `phux`, `phux new` without
  `--json`) — `telemetry::init_client()`. Logs to a **file only**: the
  attach loop owns the alt screen, so a stray log line corrupts the
  display.
- **Cockpit** (the native macOS app) — one file,
  `~/Library/Logs/Phux Cockpit/cockpit.log`; `PHUX_COCKPIT_LOG` names a
  different file. Launch Services gives the app no terminal, so at
  startup Cockpit points its standard error at that file and the Rust
  bridge installs its `tracing` subscriber on the same descriptor. The
  file therefore holds everything the process says: Cockpit's own log
  lines, the bridge's remote-tunnel lifecycle (resolve, dial, handshake,
  relay, close, and every failure reason) at info with quinn and rustls
  at warn, Rust panic text, and the Zig fatal-signal handler's report.
  `PHUX_COCKPIT_LOG_FILTER` is a `RUST_LOG`-style filter for the bridge's
  part. A file over 8 MiB rotates to `cockpit.log.1` at the next launch.
  `phux logs --cockpit` tails it.

### Crash reports

A fatal Cockpit signal leaves the Zig handler's final report in the log and
a macOS crash report at
`~/Library/Logs/DiagnosticReports/phux-cockpit-<date>.ips`.
Read `Triggered by Thread` and that thread's first stack frames to locate
the fault. Console.app lists the same files under Crash Reports.

### Local bug reports

A TUI `report-bug` (`C-a B`) or `phux report new` writes one directory under
`$XDG_STATE_HOME/<profile-dir>/reports/`. Each bundle is owner-only (`0600`)
and holds `report.md` (the file to hand an agent), `meta.json`, log tails, and
an optional screen dump. `latest` points at the newest. `phux report` lists
them; `phux report show` prints one. Nothing leaves the machine.

Both fmt layers emit span-close timing (`FmtSpan::CLOSE`), so any
`#[instrument]` span reports elapsed duration at close.

### Performance observability

Hot-path stages record always-on, lock-free histograms and counters. Nothing
needs enabling before a slowdown; the observations remain available afterward.

```sh
phux perf              # lifetime table since the server started
phux perf --watch 1    # one interval per second: rates and per-second percentiles
phux perf --reset      # snapshot, then zero, so the next call is an interval
phux perf --json       # the raw PerfReport (schema_version inside)
```

The table is grouped by pipeline stage, top to bottom in the order bytes
move. Counters show `count` / `rate/s`; histograms show sample count,
rate, `total` (sum in the declared unit), and `p50` `p90` `p99` `max`.
Latencies are elapsed microseconds, sizes bytes. Nested stages overlap:
their totals are not additive CPU time. The header's process CPU comes
from `getrusage`, independently of those elapsed durations.

Percentiles use log-linear bucket upper bounds (up to 12.5% above a
sample); `max` is exact in a lifetime report and a bucket bound in a
`--watch` interval. Reports aggregate each process's activity, not individual
panes. Use the existing debug spans to correlate a slow operation with its
pane; no terminal contents become metric labels.

| Group | What it measures | Healthy on a laptop over the local socket |
|---|---|---|
| `pty.read.size` | bytes per `read(2)` from a PTY. macOS caps this at 1024 | a percentile bound of 1151 can represent 1024-byte reads; this is the OS plus histogram resolution, not oversized reads |
| `pty.reader.blocked` | reader thread parked because the actor's queue was full | 0; anything else means the actor is behind the child |
| `pty.queue_wait` | reader-to-actor queue delay | p99 under 1 ms |
| `pty.burst.bytes` / `pty.burst.chunks` | how many reads the actor coalesced into one parse and one frame | chunks above 1 under a flood is coalescing working |
| `pty.vt_apply` / `pty.vt_parse` / `pty.post_apply` | whole synchronous ingest, canonical VT parsing, and derived work (color replies, input-mode publication, semantic events, native capture start); native replay excluded | compare parse versus post-apply before attributing a slow ingest to libghostty |
| `echo.server` | key or paste handed to the PTY writer until the next output from that pane, sampled only when the pane was quiet for the previous 100 ms; includes the child's own reaction | p50 under 1 ms for a shell prompt |
| `input.pty_write` | `write(2)` plus flush on the writer thread | p99 under 200 us |
| `input.credit_waits` | input events that waited for a saturated pane to drain before the server read the client's next frame ([ADR-0144](./adr/0144-input-credits-backpressure-instead-of-drop.md)) | 0 while typing; a burst or paste into a slow program may wait |
| `input.writer.queue_wait` / `input.writer.full` | final writer handoff to writer pickup, and full-queue refusals, including terminal-generated replies | high wait with cheap `input.pty_write` points to queued work or scheduling |
| `input.credit_wait` / `input.credit_timeouts` / `input.canonical_refused` | actual credit-wait duration and timeouts; canonical-mode writes refused before delivery | immediate credit acquisition and already-stalled fast refusals add no wait sample |
| `pty.yield_wait` / `runtime.detect_tick_late` | capped-burst cooperative yield/resume delay; lateness of the already-running detector timer | includes runtime/OS scheduling, preceding actor work, priority starvation and timer resolution; not a new heartbeat or pure CPU measurement |
| `agent.detect` / `agent.viewport` / `agent.publish` / `proc.cwd_query` | detector tick, requested viewport projection, synchronous metadata/hook enqueue, and child kernel cwd lookup | separates agent/control-plane work from parsing; hook subprocess execution is excluded |
| `tick.emit` / `tick.synth` / `tick.out_bytes` | state-sync fan-out: whole tick, per-consumer diff, per-consumer frame size | tick p99 under 5 ms; grows with consumers x rows |
| `consumer.mailbox_full` | ticks that skipped a consumer whose outbound queue was full | 0; a steady rate is a client that cannot drain |
| `consumer.ack_rtt` | emit to `FRAME_ACK` round trip per state-sync client | tracks the link: sub-ms local, tens of ms over QUIC |
| `pump.frames` / `pump.bytes` / `pump.frame.bytes` | raw broadcast fan-out volume and per-frame size | frame size near `pty.burst.bytes` |
| `pump.lagged` / `pump.gap_resync` | output pumps that fell behind (more than 256 frames, or a chunk older than 250 ms when dequeued), and the resyncs that cost; each resync re-bootstraps only the pump that fell behind | 0 |
| `wire.write` / `wire.write.bytes` / `wire.bytes_out` | coalesced socket writes per client | p99 under 500 us on UDS |
| `wire.encode` / `wire.batch.frames` | admitted frame encoding/compression and encoded frames per coalesced write batch | high encode time with cheap writes is CPU-side work; revoked/write-failed batches still count as encoded |
| `cmd.handle` / `attach.handle` | control-plane latency | attach p99 under 100 ms with a warm history |
| `attach.capture_wall` / `attach.publish_wall` | non-deferred attach staging (actor queues/capture/adaptation) and queueing `ATTACHED` through `ATTACH_READY` | includes async waits, not socket drain; distinguishes capture cost from outbound backpressure |
| `bootstrap.synth` / `bootstrap.native_begin` / `bootstrap.native_step` | synthesized replay, native capture start, and productive native capture steps | includes attempted failures; native step excludes inter-turn scheduling but includes completion replay |
| `proc.*` | clients, panes, sessions (gauges) and, in the header, CPU split, peak RSS, context switches | idle CPU under 1 percent with agents running in panes |

`GET_PERF` and `phux perf --json` also carry a `stream_diagnostics`
snapshot for the QUIC mux: up to 128 streams, each identified only by numeric
`connection_id`, QUIC `stream_id`, and a `control` or `terminal` lane, with
binding state, admitted queue bytes and oldest age (from transport admission
to dispatch or cancellation), write durations, and bind-to-READY latency.
Storage is bounded (256 queued items per stream) and overflows are counted;
no payloads or caller-defined labels are retained, and `--reset` clears
interval observations without invalidating live trackers.

The TUI client keeps its own table. Normal detach and attach-loop errors
write `session perf:` with a complete JSON `perf` field to the client log
(`phux logs --client`), after stopping/joining the stdout writer. Enable
`PHUX_RENDER_PROF=1` to record the same table as interval JSON at most once
per second while processing frames. Idle clients do not wake just to report;
there is no per-frame report allocation.

| Client rows | Boundary |
|---|---|
| `paint.full` / `paint.chrome` / `paint.submit` | composition versus synchronous sink submission; production tty I/O runs separately |
| `paint.pane` / `paint.prepare` / `paint.rows` | every pane-render attempt (including clean/error paths), pooled libghostty render-state preparation, and dirty-row walking/front-buffer diffing/VT emission; nested elapsed times, excluding composite chrome, submission and tty I/O |
| `loop.input_wall` / `loop.frames_wall` | selected stdin/frame handler through completion, including awaited sends/errors; excludes parking in `select!` |
| `loop.burst_frames` / `loop.burst_capped` | inbound batch size and fairness-cap hits |
| `pacer.hold` / `pacer.late` / `pacer.panes` | first withheld output to debt retirement, lateness versus pacing deadline, and panes retired; not a receipt for pixels or tty delivery |
| `stdout.backlog` / `stdout.queue_wait` | queued bytes after submission admission or overflow discard (excluding writer in-flight chunks), and each written chunk's enqueue-to-write-start delay; discarded chunks add no wait sample |
| `stdout.write` / `stdout.flush` / `stdout.errors` | real writer-thread calls, including blocking and error paths; idle waiting is excluded |
| `stdout.bytes` / `stdout.written` / `stdout.dropped_bytes` | offered bytes, completed successful writes, and overflow discards including the trigger frame; failed partial writes and shutdown discards are not counted as successful delivery |

Degradations such as a full consumer mailbox or dropped stdout backlog warn
at most once per ten seconds with a `suppressed` count.

A native embedder such as Cockpit reads its client's table with
`phux_client_perf_json`: the kernel's `kernel.*` rows (frames and bytes
applied, `kernel.apply`, `kernel.echo.rtt`) and the runtime's `runtime.*`
rows (`runtime.apply_batches` owner-thread round trips, `runtime.publish`
grid publications, `runtime.project` projection time, and the read pacing
behind them: `runtime.acquire` consumer reads, `runtime.publish_deferred`
projections skipped because the current frame was unread, `runtime.catch_up`
projections a read pulled). `kernel.frames` divided by `runtime.publish` is
how many output frames one publication absorbed. Under a flood
`runtime.publish` should track `runtime.acquire`; one well above it means a
consumer reads every frame it is woken for rather than once per display
tick.

The runtime also splits owner-thread work into `runtime.owner_queue_wait`
(command enqueue to dequeue), `runtime.owner_execute`, and
`runtime.request_wall` (primary request-channel creation through reply/error).
Explicit-view secondary response-channel overhead is outside `request_wall`.
`runtime.apply_execute` includes publication; `runtime.apply_events` counts
delivered events, including an unapplied suffix after a fatal event;
`runtime.catch_up_execute` covers deferred projection on acquire.
`runtime.project_grid` separates projection/prediction from
`runtime.publish_swap` (slot swap and predecessor reclamation).
`runtime.buffer_held` counts predecessor frames that a consumer still owns
and therefore cannot be recycled. `runtime.request_errors` counts owner
channel send/receive failures, not business-operation rejections such as an
unknown view. `runtime.apply_errors` and `runtime.project_errors` count failed
apply events and timed projections respectively; projection setup is excluded.

The desktop host (`clients/desktop`) appends its painter rows to the same
report, `desktopPerfJson`. Per terminal element per window draw:
`desktop.render`, `desktop.acquire` (frame validation, one owner round trip),
`desktop.acquire_rejected`, `desktop.prepare` (with `desktop.prepare_reused`
for draws that reused the previous scene and `desktop.shaped` line shapes
for those that did not), `desktop.paint` and `desktop.present`; and
`desktop.key_to_paint`, from a key reaching a focused terminal to the first
paint of that terminal's next output. Launched with
`PHUX_DESKTOP_PERF=<absolute path>`, the desktop appends one JSON line per
second with that report, the main window's draw count and recent draw times,
its wake drains (count, events, milliseconds spent applying them, wakes
deferred to the next display frame), the main
window's GPUIX mutation batches by kind (each batch redraws the window), and
current memory. `bun clients/desktop/scripts/perf-bench.ts` drives a fixed
workload (idle, background-tab flood, one-pane and every-pane floods) against
a private server and summarizes those lines per phase, plus a `vmmap` region
summary; compare runs made back to back on one host.

For a reproducible number rather than a live one, `just perf-echo` runs
the byte-level echo probe against an isolated server at a chosen size
with a flooding sibling pane. Its server snapshots bracket the measured
phase after command-proven readiness and before detach, not fixed sleeps.
It retains raw snapshots, their interval table, echo samples and the client
log in the printed artifacts directory. Readiness duration is recorded
separately; a prompt-string timeout is no longer required before the
readiness command. Collection failures retain the samples and make the load
harness fail rather than silently report missing telemetry.
The sibling flood emits no visible probe letters, including its numeric
frame footer, so it cannot satisfy a key-echo match by itself.

Timing runs do not invoke a sampling profiler. Set `PHUX_BENCH_SAMPLE=1`
for optional macOS server/client `sample` profiles; profiling adds overhead,
so do not compare those timings with unsampled runs. `just profile` records
the `profiling` build (symbols kept) with samply.

To isolate pane emission from tty scheduling, run
`cargo bench --locked -p phux-tui --features testkit --bench render_frame`.
The existing deterministic corpora exercise full-dirty, one-row-dirty and
clean frames, reporting time, bytes, Rust heap allocations and flushes per
frame. This excludes libghostty's C allocator and physical display latency.


### Environment knobs

| Variable | Effect |
|---|---|
| `RUST_LOG` | Filter directives. Default `phux=info,warn`. |
| `PHUX_LOG=<path>` | Write logs to `<path>` via non-blocking file writer. Server tees to this file *in addition to* stderr; client writes here *instead of* its per-pid default. Parent directory created if missing. |
| `PHUX_LOG_FORMAT=text\|json` | `text` (default): human single-line layer. `json`: one JSON object per line for `jq`/`grep`. Applies to both stderr and file sinks. |
| `PHUX_RENDER_PROF=1` | Client only. At most one `render_prof` INFO line per second while processing frames, with paint/echo counters and the complete interval `PerfReport` in the JSON `perf` field, including handler, pacer and stdout timings. Disabled, no periodic report is allocated. |
| `PHUX_FRAME_INTERVAL_MS=<ms>` | Client only. Minimum interval between composited frames; default `16` (one frame at 60Hz). The first frame after any lull always paints immediately, and output from the pane the user last acted on is never paced while its reply grace is open, so this only bounds how often a sustained *unsolicited* output stream repaints. `0` disables pacing entirely. |
| `PHUX_INPUT_GRACE_MS=<ms>` | Client only. Pins how long after input that pane's output still counts as a reply and bypasses pacing. Unset, it is `max(20ms, 2 x observed input-to-output latency)` capped at 250ms, keyed to the pane the input went to; pointer motion does not arm it. `0` paces every frame. |
| `PHUX_TTY_READINESS=0` | Client only. Read the outer terminal through tokio's blocking-pool stdin instead of reactor readiness on its own non-blocking handle. Use it when a pollable terminal misbehaves; unpollable ones fall back automatically. |

The **canonical server log** is `$XDG_STATE_HOME/phux/server.log` (falls
back to `$HOME/.local/state/phux/`). The auto-spawned daemon and the
`phux service` unit both write it, and every reader resolves it through
`phux_server::telemetry::server_log_path`. The **client default log** (when
`PHUX_LOG` is unset) is `client-<pid>.log` in the same directory, at
`phux=info,warn`. The non-blocking file writer flushes on normal exit; an
`abort` skips that, which is why the panic hook also writes synchronously.

### Sensitive data in logs

Unix log sinks use owner-only mode `0o600`. `KeyEvent` and `PasteEvent` have
hand-written `Debug` implementations, and `InputEvent::narrate` reports only
action, physical key, modifiers, and payload lengths. Neither typed text nor
pasted bytes appear, including in `trace!(?input, …)`.

### Remote WebSocket pairing admissions

Remote pairing diagnosis is visible in the default log without exposing
request material:

- Every peer-caused TLS, pairing-authentication, or WebSocket-upgrade
  rejection is one `DEBUG` event with a safe `stage` and `source_ip`. A
  single listener-wide limiter (no per-IP map, so source churn cannot grow
  memory) also emits a `WARN` at most once per 60 seconds with the latest
  stage and a `suppressed_count`.
- Rejection errors are a concrete safe type: they drop the ephemeral port
  and the underlying TLS/HTTP/WebSocket error so URIs, headers,
  certificates, and tokens cannot be formatted. Missing, malformed, unknown,
  and revoked tokens share one message and one HTTP 401.
- Listener and resource failures (such as accept exhaustion) keep the shared
  accept loop's single `ERROR`.
- A token-authenticated WebSocket admission is one `INFO` event with only
  `transport=ws`, `source_ip`, and the non-secret `credential_id`. Loopback
  and other-transport admissions stay at `DEBUG`.

These are connection diagnostics, not an audit log. `PeerIdentity` is never
logged wholesale.

### Crash capture

The client panic hook writes the message and captured
`std::backtrace::Backtrace` to its file before restoring the terminal, so
the backtrace survives leaving the alternate screen. The server hook records
task/actor panics and backtraces through `tracing`, including when daemonized.
Both honor `RUST_BACKTRACE`.

The server hook is armed only by the long-running daemons (`phux server`,
`phux relay run`); one-shot CLI verbs keep the default hook so a CLI crash is
not misreported as a server panic. Because release builds use
`panic = "abort"`, the server hook also appends the record to `PHUX_LOG`
synchronously (mode `0o600`); a debug build may therefore show it twice.
Release builds keep line tables (`debug = "line-tables-only"`,
`strip = "none"`) so backtraces resolve to functions and lines.

### Blast radius of a panic

The release build uses one server process per user
([ADR-0003](adr/0003-server-process-model.md)), one current-thread runtime,
and `panic = "abort"`. A panic anywhere in the daemon ends every session,
window, and pane in that process. Logging the panic does not contain it.

Planned upgrades can preserve live work through
[update handoff](#workspace-continuity-and-update-survival). A crash cannot:
only the crash record survives. Aborting avoids partially unwound actor
state and reduces binary size by omitting unwind landing pads.

### Reading a trace to localize lag

The hot paths carry `tracing` spans whose `CLOSE` event reports the
span's duration (`time.busy`/`time.idle`), so a captured session shows
where time went before a stall. The per-frame and per-tick spans are at
**debug**, so the default `phux=info` filter leaves them off and
effectively free; raise the level only while diagnosing.

```sh
PHUX_LOG=/tmp/phux.jsonl PHUX_LOG_FORMAT=json RUST_LOG=phux=debug phux ...
```

Two spans carry most of the signal. On the server,
`synthesize_against_reference` (fields `changed_row_count`, `out_bytes`)
is the per-tick CPU cost of diffing engine state for one consumer. On the
client, `handle_server_frame` (grep `kind=terminal_output`) is the
per-frame apply-and-paint cost; its children `vt_apply` (libghostty
parse) and `paint_trigger` (render) let you attribute a client stall to
parse versus paint by comparing their `time.busy`. Narrow a JSON capture
to timed events with `jq -c 'select(.fields.message=="close")'`. Finer
per-PTY-chunk and per-frame-emit detail is at **trace**; a wedged or
leaked consumer shows as `consumer mailbox full` / `consumer mailbox
closed` at debug.

### Finding and tailing the logs

`phux logs` lists the server log, per-PID client logs newest first, their
state directory, and the Cockpit log on macOS, with existence, size, and age.
Missing files are marked "not created yet", not treated as errors.

Use `--server`, `--client`, or `--cockpit` to tail a log. `--client` selects
the newest unless `--pid PID` is supplied; `-f` follows and `-n NUM` sets the
tail length. `--json` emits a stable `schema_version` 1 inventory
(`cockpit_log` is `null` off macOS). `phux service logs` tails the same server
log as `phux logs --server`.

There is no Prometheus/OpenTelemetry exporter, or runtime per-target
log-level control. Use `phux status` and `phux ls --json` for the running
view and the environment-controlled tracing sinks above for diagnosis.
There is no access audit log.

## Voice

`TRANSCRIBE` runs the argv in `[voice].transcriber` on an uploaded clip
and pastes stdout into the pane. Without `[voice]` the command is
refused. Only an audio upload (`wav`, `m4a`, `aac`, `caf`, `mp3`, `ogg`,
`oga`, `opus`, `webm`, `flac`) reaches the transcriber: ffmpeg-based
tools follow the paths inside playlist and concat formats, so any other
extension is refused before a process starts. Schema and defaults:
[`docs/reference/config.md`](./reference/config.md).

Uploads (`PUT_FILE`) land in `PHUX_UPLOAD_DIR`
(`$XDG_DATA_HOME/phux/uploads` by default). An upload id belongs to the
connection principal that started it: its pairing or workload credential,
else the owner socket's uid. The directory is held to
`PHUX_UPLOAD_MAX_BYTES` (8 GiB) and `PHUX_UPLOAD_MAX_FILES` (10,000
uploads); past either, a new upload is refused with the remedy, and `0`
lifts a limit. Finished uploads are never deleted by the server, so a
full directory needs `rm` of old `phux-upload-*` files. Partial uploads
untouched for a day are swept at startup and on every upload.

## Agent-state detection

The server derives each pane's `phux.agent/v1` record from that pane's
PTY (foreground argv basename) plus the OSC title and viewport rows
already in engine state. Screen content is not logged. A bad rule file
is logged at `warn` and dropped whole. `PHUX_AGENT_DETECT=0` in the
server's environment loads no rules, so nothing is scanned. Shims,
`phux agent show`, and the detector live in
[`consumers/agents.md`](./consumers/agents.md).

## Server lifetime

A phux server stops on exactly three conditions. The first two are the
defaults; the third has to be asked for.

1. **Signalled.** Ctrl-C on a foreground `phux server`, or any signal
   that ends the process.
2. **Last pane reaped**, once at least one client has been served. When a
   pane's process exits the runtime reaps it, cascading to its window and
   session; an empty server exits. The "served a client" guard exists so
   a freshly auto-spawned server whose seed pane dies before anyone
   connects stays up long enough for the launching `phux` to repopulate
   it.
3. **Unattended past `--exit-after-idle SECS`**, if that flag was passed.

`phux server --exit-after-idle SECS` is for **ephemeral** servers: a test
harness or CI job that bootstraps a private server per run on a temp
socket and cannot guarantee its own cleanup step will execute. Such a
server exits once no client has been *connected* for `SECS` — live panes
and all. Both "nobody ever connected" (the clock runs from startup) and
"the last client left" are covered.

- **Connected, not attached.** One-shot control verbs (`phux ls`,
  `phux send-keys`, `phux new --json`) count: each connect postpones the
  exit. A scripted harness that never attaches is safe.
- **Quiet does not mean idle.** An open connection pins the server alive
  no matter how long it sends nothing, so an attached human is never
  reaped.
- **The exit is the graceful one.** It runs the same teardown as Ctrl-C:
  each pane's process group is SIGHUPed, given a grace period, and the
  child is reaped; the socket is unlinked.
- **It survives `phux upgrade`.** The lifetime is re-passed on the
  re-exec, so upgrading an ephemeral server does not make it permanent.
- **It is not a config default and should not become one.** A server a
  human attaches to must keep the multiplexer contract.

```sh
# A private, self-terminating server for a scripted run.
phux server --session ci --socket /tmp/phux-ci-$$.sock --exit-after-idle 120 &
```

## Instance isolation (profiles)

Each phux process resolves a profile that separates development sessions
from the installed release's sessions
([ADR-0080](adr/0080-socket-lifecycle-and-instance-isolation.md)):

| profile | when | socket | state |
|---|---|---|---|
| `default` | an installed release | `/tmp/phux-$USER/phux.sock` | `$XDG_STATE_HOME/phux` |
| `dev` | a `target/` or debug build | `/tmp/phux-$USER-dev/phux.sock` | `$XDG_STATE_HOME/phux-dev` |
| *name* | `PHUX_PROFILE=name` | `/tmp/phux-$USER-name/phux.sock` | `$XDG_STATE_HOME/phux-name` |

`$XDG_RUNTIME_DIR/phux[-<profile>]` replaces the `/tmp` path when that
variable is set, and `PHUX_SOCKET` (or `--socket`) still overrides
everything. Paths in full:
[`docs/reference/files.md`](./reference/files.md).

A binary under a Cargo `target/` directory or built with `debug_assertions`
automatically selects the development profile. Set `PHUX_PROFILE` explicitly
for additional instances, such as one per agent worktree.

`cargo run` therefore does not show your installed phux's sessions.
`phux doctor` reports non-default profiles:

```
warn instance  profile dev (this is a development build …); state …/phux-dev
```

### Hard guards: a dev build never reaches production

Profiles set default paths, which explicit socket/profile settings or a copied
binary could bypass. Hard guards enforce the production boundary with no override:

- **Connect and bind.** A development build refuses to connect to, bridge
  to, or bind a production socket: `/tmp/phux-$USER/phux.sock`,
  `/run/user/<uid>/phux/phux.sock`, or `$XDG_RUNTIME_DIR/phux/phux.sock`
  when that directory is not inside the temp directory (test harnesses pin
  the released layout in a temp sandbox, which is not production). Paths
  are compared after resolving symlinked directories. Every local client
  connection, `phux stdio-bridge`, and the server's bind go through
  `phux_config::socket::refuse_dev_on_production`.
- **Hot swap.** `phux upgrade` re-execs whatever binary sits at the
  server's installed path. Before it does, the server asks that binary what
  it is (`PHUX_PROBE_BUILD_KIND=1 phux` prints `release`, `local`, or
  `dev`). A non-dev server refuses to become a dev build, and a stamped
  release server refuses anything but another stamped release. Release
  artifacts are stamped by `scripts/build-release-binaries.sh`
  (`PHUX_RELEASE_ARTIFACT=1`), the only script the release workflows
  build with. Switching builds on purpose is a restart, not an upgrade.
- **Production state.** A development build refuses to write production
  phux state, or to read a production secret. A production pane exports
  `PHUX_WS_TOKENS`, `PHUX_WS_TLS_CERT`, and `PHUX_WS_TLS_KEY`, so anything
  run from one inherits the operator's credential store and certificate;
  this guard is what stops a dev build from minting into them. Production
  means the default profile's real locations: `phux` (never `phux-dev`)
  under `~/.local/state`, `~/.config`, and `~/.local/share`, the
  `com.phux.server` launchd plist and the `phux.service` systemd unit. The
  home directory is the account's (from the password database) whatever
  `$HOME` says, plus `$HOME` and `$XDG_STATE_HOME` / `$XDG_CONFIG_HOME` /
  `$XDG_DATA_HOME` unless they sit inside the temp directory, where test
  harnesses pin the released layout. Paths are compared after resolving
  symlinks, and nothing overrides the refusal. Every writer goes through
  `phux_config::production::refuse_dev_on_production_state`:
  - `phux` itself, at startup, when the state directory is production
    (`PHUX_PROFILE=default` outside a sandbox): nothing runs;
  - the credential store (`pair` mint, rotate, revoke, prune, migrate; the
    server's last-seen writes), the TLS pair (provisioning, and a server
    presenting it: its remote listeners stay off), workload authority
    material (every write, and reading the CA key), and a connector's
    consumer tokens;
  - `phux relay` state, which is not profile-scoped;
  - the `[[remote]]` / `[[satellites]]` and plugin registries in
    `config.toml`, and the plugin directory (other settings in the shared
    `config.toml` stay writable, since profiles do not split it);
  - service units (`phux service install`, `uninstall`, `reconcile`, and a
    hub patch), including a dev unit that would carry production
    credential paths;
  - the server's upload directory, also not profile-scoped.

  `phux update` needs no entry: it already refuses a binary under a Cargo
  `target/` directory, and its server hand-off goes through the socket
  guard.

Contributors and agents must test against a dev server; `just rebuild`
hot-swaps only the dev-profile server. Never copy a build into an install
location such as `~/.local/bin` or point it at production sockets or state.

Tests spawning `phux` scrub inherited `PHUX_*` variables
(`crates/phux/tests/common/ambient.rs`). In-process servers read configuration
from `ServerConfig::env`, empty by default, rather than the process environment.
Only `phux server` fills it with listener, TLS, credential, workload, upload,
and SSH settings. Do not route around the production guards.

## Restart policy and crash-loop visibility

The generated unit restarts the server on **abnormal exit only**,
throttled to one start per 30s:

| | launchd | systemd |
|---|---|---|
| restart when | `KeepAlive{SuccessfulExit:false}` | `Restart=on-failure` |
| throttle | `ThrottleInterval 30` | `RestartSec=30s` |
| give up after | *(no such knob)* | `StartLimitBurst 5` / `StartLimitIntervalSec 180s` |

`ThrottleInterval` and `RestartSec` only space starts; systemd's start limit
is the bound, and launchd has none, which is why `phux service install`
refuses when a server already holds the socket.

- **A deliberately stopped server stays stopped.** `phux kill --server`
  stops the server over the wire, so it exits cleanly and the supervisor
  leaves it alone. A server killed by a signal counts as abnormal under
  launchd and comes back. The next `phux attach`/`phux new` auto-spawns a
  fresh server.
- **A crash-loop is visible.** Every server start appends to
  `$XDG_STATE_HOME/phux/server-starts.log`, and `phux doctor` *fails*
  `server-health` at 5+ starts in an hour, pointing at `server.log`.

`phux doctor` also warns when the installed unit predates this policy;
re-running `phux service install` replaces it.

### Upgrades and version skew

`phux update` asks the running server to re-exec in place (the listening
fd is passed to the new image, so panes and scrollback survive). Before
that re-exec, the new image must pass `config check`; a broken `extends`
layer aborts the upgrade and the old server keeps serving. The same
command rewrites an installed service unit's binary path to this
install, so a leftover Homebrew `ProgramArguments` cannot leave launchd
pointing at a file that no longer exists. Package managers bypass the
re-exec: `brew upgrade phux` swaps the binary while the old server keeps
running, indefinitely.

The server's start history records its build version, which the protocol
handshake does not negotiate. Attach detects a mismatch and initiates handoff:

```
phux: the running server is 0.13.0, this binary is 0.14.0 — upgrading it in place
```

`phux doctor` reports the same skew if you want to check without
attaching.

**Keep-empty sessions and downgrades.** The handoff carries each
session's keep-empty mark, so an upgrade keeps it. Handing off to an
*older* binary that predates the mark drops it: a populated keep-empty
session falls back to the default cascade, and an empty one comes back
with no windows and no mark, which that older server has no way to remove
except `phux kill --server`. Kill empty sessions (`phux kill NAME`)
before downgrading.

### Putting a server that is already running under supervision

`phux service install` refuses while a server holds the socket: a second
server would fail to bind and keep retrying. Use `--adopt` to prepare
supervision without stopping the running server:

```
$ phux service install --adopt
phux service armed (nothing was stopped).
  unit    ~/Library/LaunchAgents/com.phux.server.plist
  panes   untouched — the running server was not signalled
```

The command writes the unit from your flags and arms it without starting
another process. Supervision begins at the next login or reboot, or at the
first `phux` command after the current server exits, whichever comes first.
That command starts the supervised server instead of an unsupervised one.

Neither launchd nor systemd can adopt an existing process for restart
supervision; `--adopt` leaves it and its panes untouched. `phux service status`
reports `state armed` while waiting, and `phux service uninstall` cancels it.
If nothing is listening on the socket, `--adopt` performs an ordinary install.

### Scheduling class

The server is the keystroke path between the user and every pane, so it
runs in the interactive scheduling class, not the batch one. On macOS
every thread that carries input or its echo — the runtime thread, each
PTY reader and writer, the input lane, the client's attach loop and
stdout writer — requests `QOS_CLASS_USER_INTERACTIVE` for itself at start
(no privilege needed), and the launchd unit `phux service install`
writes declares `ProcessType` `Interactive`. `phux perf` reports the
result as `proc.sched_interactive` (`1` granted, `0` not). A `Background`
unit lets launchd throttle the keystroke path under load; `phux service
reconcile` (run after an update) rewrites an installed `Background` unit to
`Interactive`, effective at the next launchd start. Linux has no unprivileged
equivalent, so the request is a no-op there and the gauge reads `0`.

## Service-managed pane environment

launchd and systemd start service units with a minimal environment, without
sourcing a login profile. Homebrew or Nix additions to `~/.zprofile` or
`~/.profile` would otherwise be absent from each pane's `PATH`.

`phux service install` therefore writes `PHUX_SERVICE_MANAGED=1` into the
unit (`EnvironmentVariables` on launchd, `Environment=` on systemd).
At startup, `phux server` checks this marker. When present, command-less
panes use their shell's login mode:

| shell        | login flag |
|--------------|------------|
| `bash`       | `-l`       |
| `zsh`        | `-l`       |
| `fish`       | `--login`  |
| `sh`         | `-l`       |

Any other `defaults.shell` gets no login flag, since its flag semantics are
unknown. A hand-started or auto-spawned server never carries the marker and
keeps plain, non-login panes: that environment is already
profile-initialized, and re-sourcing is not idempotent for every setup
(`nvm`/`rbenv`/`direnv`). The server reads the marker once at startup, so an
older unit needs `phux service install` rerun. The installer never freezes
its own transient `PATH` (for example from `nix develop`) into the unit.

## Workspace continuity and update survival

Workspace restore and live update handoff have different guarantees:

- **Restart restore:** `phux workspace save` writes a typed JSON archive of
  the running workspace, reading each session's split tree from its L3 layout
  envelope (`phux.tui.layout/v1/<session-id>`, or `save --projection KEY`);
  a session with none falls back to one pane per window. `phux workspace
  restore ARCHIVE` recreates missing sessions on a running server with a fresh
  PTY per archived pane (the archived `command`, a resumable native agent
  session, or the default shell in the archived cwd) and replays the split
  tree with fresh window identities, confirmed by read-back. A session that
  fails partway is rolled back and named; the rest still restores, and the
  command exits non-zero if any session failed
  ([ADR-0129](./adr/0129-projections-are-named-by-key.md)). A native agent
  session is archived for a `phux launch` pane and for an agent started in a
  plain shell whose `AgentSession` provider an enabled integration claims and
  can resume; save warns about any agent pane that will come back as a shell
  ([ADR-0151](./adr/0151-live-agent-sessions-bridge-into-native-restore.md)).
  `--output` writes a temp file, fsyncs it, and renames it into place, so an
  interrupted save never replaces a good archive with a torn one.
- **Crash-safe autosave:** `phux service install --restore` runs the
  supervised server with `phux server --autosave
  <state-dir>/workspace.json`. On a fresh start the server restores that
  archive once it is listening; afterwards it rewrites it atomically about
  three seconds after a session, window, pane, name, cwd, layout, or
  agent-session change (at most ten seconds behind a workspace that never
  settles), and at least once a minute for drift such as titles. A crash,
  `SIGKILL`, abort, or power loss therefore restores the latest layout, and
  the wrapper still saves once more on a clean stop. Shutdown never
  autosaves a half-torn-down workspace, a hot upgrade keeps saving without
  restoring again, and an archive that fails to restore is copied to
  `workspace.json.unrestored` and left alone until the workspace changes. A
  unit installed before this needs `phux service install --restore` rerun
  ([ADR-0150](./adr/0150-the-server-keeps-the-restore-archive-current.md)).
- **Live update handoff:** `phux upgrade` keeps existing PTYs alive
  across a server binary re-exec, and with them each pane's agent sessions:
  same `@N`, same `phux agent log` history, same record sequence
  ([L1 §4.8](./spec/L1.md)). `phux update` is the user-facing verb
  built on that handoff: it resolves the published release, verifies the
  `.sha256` sidecar, replaces the binaries atomically, then calls
  `phux upgrade`. Default is the latest `vX.Y.Z`; `phux channel next` follows
  green `main`, and `phux channel latest` returns to numbered releases. It writes only to installs it maintains — a Homebrew,
  Cargo, or Nix install gets the exact native command instead
  ([`INSTALL.md`](./INSTALL.md#updating)).

The compatibility unit is the release: a server, its local clients, its
satellites, and its relays must all run the same one, because a wire
`minor` bump refuses mismatched peers at HELLO with no grace window.

## Security model and trust boundaries

**Design assumption:** This is not a security-hardened system for hostile
environments. It is suitable for trusted networks and multi-user boxes
where Unix permissions are enforced by the kernel.

The trust boundary is the operating system user. A phux server trusts
every process running as the same UID that can connect to its Unix
socket.

### Local trust model (single-machine)

The Unix socket lives in `$XDG_RUNTIME_DIR/phux/` (typically
`/run/user/$UID/` on Linux, or `/var/folders/.../T/` on macOS), created
with parent directory mode `0o700` (user-only). The OS kernel enforces
this boundary at the filesystem level; the socket inherits the parent
directory's permissions.

**What this means:**

- Another user on the same machine MAY NOT connect to the socket
  (kernel-enforced).
- If the parent directory or socket permissions are misconfigured (e.g.,
  accidentally mode `0o777`), the security boundary is breached.
  **Administrators MUST validate socket permissions in deployment; phux
  does not re-check at runtime.**
- The process file descriptor table (`/proc/<pid>/fd/<socket-fd>` on
  Linux) is not readable by other UIDs, so the socket endpoint cannot be
  enumerated across user boundaries.

**Directory listing.** The L3 `LIST_DIRECTORY` query lets a connected
client list the child directories of any path the server's OS user can
read. It returns names and a symlink flag only, never file contents, and
it cannot write. It exposes nothing a client could not already learn by
spawning a shell as the same user. A hub does not route it to a
satellite. The walk is bounded (1024 returned entries, a 5 s reply
deadline, at most 8 concurrent listings); a hung mount costs a bounded
number of stuck workers rather than an unbounded leak.

**Identity report.** `phux whoami` prints who the asking connection is:
the bearer credential's principal and id, or a socket client's kernel
peer uid, plus the auth route and the OS user and host the server runs
as. The server does not switch users. Reaching another user means
reaching that user's own server. The record carries no secret; the
credential id is the same non-secret id `phux pair rotate` takes.

### Federation trust model

Remote attach is available over WebSocket/TLS, QUIC/TLS, WebTransport,
and SSH-stdio. Satellites are phux servers on other machines. A server
started with `--hub` dials enabled `[[satellites]]` and routes
host-qualified operations over the same wire. Routes are hub-and-spoke;
remote sessions and windows are not merged. Enrollment is
[Remote access](./remote-access.md): `phux host add --role satellite HOST`
installs the satellite's per-user service, registers it here, and enables
local `--hub` without dropping listeners already baked into the unit.

- **WebSocket/TCP:** `phux server --listen HOST:PORT`; loopback can be
  plaintext for browser/dev use, while routable binds auto-provision TLS
  and require a `phux pair` bearer token.
- **QUIC/UDP:** `phux server --quic HOST:PORT`; always TLS 1.3. Routable
  binds use the same token store and certificate fingerprint as the
  WebSocket path.
- **WebTransport/UDP:** `phux server --webtransport HOST:PORT` (or
  `PHUX_WT_ADDR`); HTTP/3 over QUIC, always TLS 1.3. Routable binds
  require the same `phux pair` token, carried as `Authorization: Bearer
  <hex>` from native consumers or `?token=<hex>` on the session URL from
  browsers (the JS `WebTransport` API cannot set headers).
- **SSH-stdio:** `ssh HOST phux stdio-bridge` splices the wire into the
  server's Unix socket on HOST. Authentication and encryption are SSH's;
  the remote bridge is an ordinary local UDS client. There is no bearer
  token or certificate pin on this transport. Under `paired` it still
  holds owner authority: an SSH peer that can run the bridge already owns
  the host. The bridge labels the connection `ssh-stdio` in `phux whoami`,
  and that label grants nothing: its trust is exactly the UDS peer's.
  Treat `ssh_client` as a label the connecting side reported, not a
  verified address.

A satellite is authoritative for its own Terminals and nothing else. The
hub re-tags or vets every id a satellite sends before a hub consumer sees
it (L1 §9.1), so a satellite cannot name a hub window, a hub client, a
hub approval, or another Terminal's forwarded operation. It can still end
anything about its own Terminals, including declaring one closed, which
withdraws hub-held approvals naming it: those only ever refuse the held
action, never run it, and a satellite could refuse the relayed action
anyway.

### Remote consumer trust model (opt-in)

A remote consumer can attach over the network without an SSH tunnel,
behind TLS plus a bearer pairing token
([ADR-0031](adr/0031-remote-consumer-auth-and-encryption.md)). The bind
address is the toggle:

- **Loopback address → plaintext, unauthenticated.** The historical
  browser-client dev path; zero config. Browsers do not hold WebSocket
  handshakes to the same-origin policy, so the upgrade is refused (HTTP 403)
  when it carries an `Origin` that is not a loopback page: otherwise any site
  the user visits could drive this listener. Native clients send no `Origin`
  and are unaffected. `PHUX_WS_ALLOWED_ORIGINS` names more origins
  (comma-separated), or `*` behind a proxy that checks origins itself.
  A listener that admits anyone (this one, and a loopback QUIC or
  WebTransport listener) serves at most 256 connections at once; one
  past that is closed as soon as it connects, and the refusal is logged
  at a bounded rate.
- **Routable address → TLS + token, auto-provisioned.** Binding
  off-loopback is treated as exposing the server: phux generates and
  persists a self-signed certificate (under the state dir) if none is
  configured, and reads the default token store. It terminates TLS and
  requires an `Authorization: Bearer <token>` in the WebSocket upgrade; a
  missing or unrecognized token is refused with HTTP 401 before any phux
  frame is read. Plaintext never reaches a routable address. Tokens are
  minted with `phux pair`, which prints the token once alongside the
  certificate's SHA-256 fingerprint to pin out-of-band.

Native clients:

```sh
phux attach --ws wss://HOST:PORT --token HEX --cert-fingerprint FP
phux attach --quic HOST:PORT --token HEX --cert-fingerprint FP
```

`PHUX_WS_SECURE=1` forces the secure path on a loopback address;
`PHUX_WS_TLS_CERT` + `PHUX_WS_TLS_KEY` substitute an operator-supplied
certificate; `PHUX_WS_TOKENS` overrides the token-store path.

**The trust boundary widens past the OS user:** an authenticated network
peer is a first-class consumer whose proof is a bearer token over TLS.
This is a larger attack surface than local UDS. The token is a bearer
credential — anyone holding it is the device until the token is revoked.
Treat a remote token as command-execution access: an authenticated
consumer can spawn commands and drive shells with the server user's
authority.

The versioned store must be a regular, non-symlink file owned by the
effective user with no group/world permissions (normally `0o600`),
including when `PHUX_WS_TOKENS` selects a custom path. Integrity failures
deny authentication rather than retaining a stale credential. The store
retains only a verifier plus credential id, principal, terminal-only
scope, lifecycle timestamps, and rotation generation; bearer secrets are
never persisted. Pairing, rotation, and revocation take effect with no
restart, and revocation and expiry also end an established session: while a
bearer-admitted connection is live the server re-reads its store (every
250 ms, one `stat` per poll) and closes each connection whose credential
generation was revoked, removed, or expired, with `ERROR { PERMISSION_DENIED }`
and `DETACHED { AUTHORIZATION_REVOKED | AUTHORIZATION_EXPIRED }`
([ADR-0116](adr/0116-workload-auth-is-mtls.md) supersedes ADR-0031's
survive-until-drop; [workload-auth.md](spec/workload-auth.md) §7). Only a
positive verdict from a store that loaded cleanly and holds credentials ends a
live session: its generation revoked, expired, or absent. A missing, empty,
truncated, insecure, or unreadable store refuses new connections at once but
never ends a live one; the server warns once a minute, naming the condition
and its fix. A federation hub's link to a satellite is such a session, so
revoking the link's token drops every hub consumer's attach through it.
Rotation defaults to a 300-second overlap, and a live session still on the
previous generation is disconnected when the overlap ends, so a leaked old
token cannot outlive it; `phux pair rotate` says so, and
`--overlap-seconds` (up to 86400) gives devices longer to pick up the new
token. Certificate lifecycle is an operator
responsibility, like socket permissions: verifying the `phux pair`
fingerprint on the device's first connect is what closes the
trust-on-first-use MITM window. The certificate the server provisions is
issued by its workload CA, which clients pin from then on (next sections).

#### Workload mTLS (`phux workload`)

Setting `[policy] mode = "paired"` (or `PHUX_WORKLOAD_MTLS` with no mode) on
the server adds a client-certificate check
to every QUIC listener (the configured one and each `phux attach --ssh`
door) and to the WSS listener
([ADR-0116](adr/0116-workload-auth-is-mtls.md),
[workload-auth.md](spec/workload-auth.md)). A connection must then present a
certificate issued by the server's workload CA whose public key is enrolled
and neither revoked nor expired. Where the listener asks for a pairing token,
the token is still checked first, as outer admission only. With the variable
set, a loopback WebSocket listener serves TLS too, because plaintext cannot
carry the certificate check. A relay connector (`[[connector]]`) and a
WebTransport listener (`--webtransport`, `PHUX_WT_ADDR`) carry the
certificate in a TLS session the client runs with this server inside its
stream, since the relay terminates the outer TLS and a WebTransport session
has none to give; under `paired` they admit nothing else
([ADR-0154](adr/0154-devices-enroll-with-a-ticket-over-their-own-alpn.md)).
A browser cannot run that session yet, so it reaches a `paired` server over
neither. Unset, every listener and connector behaves exactly as described
above.

```sh
phux workload authority --init            # create the CA; prints only its fingerprint
phux workload add-key --scope 'observe,input@host' \
    --cert-out client.pem < client.csr    # sign a CSR read from stdin
phux workload list                        # ids, scopes, expiry, revocation
phux workload revoke sha256:...           # refuse it and end its live connections
```

The workload keeps its private key. `add-key` accepts only a certificate this
CA issued or a certificate signing request, read from stdin or `--file` and
never from the command line, and refuses input that contains a private key.
The client presents its pair through `PHUX_WORKLOAD_CERT` and
`PHUX_WORKLOAD_KEY`, which name files and never hold key bytes. Setting
`PHUX_WORKLOAD_REQUIRE_PAIRED` as well makes the client refuse a server that
does not ask for the certificate: the TLS handshake fails before the pairing
token or any phux frame is sent, so a listener that lost its workload check
cannot quietly downgrade the client to bearer-only admission
([workload-auth.md](spec/workload-auth.md) §3). It is off by default, needs
both identity variables, and disables TLS session resumption so every
handshake shows whether the server asked.

A device with no ssh to the host, such as a phone, enrolls instead with a
ticket: `phux pair --enroll` puts a single-use, ten-minute ticket in the
connect link, and the device sends it with a certificate signing request for
a key it generated (and, on a phone, keeps in its keystore) over the QUIC
listener's second ALPN, `phux-enroll/1`, which the configured listener
offers in every mode so devices can enroll before `paired` is turned on
([ADR-0154](adr/0154-devices-enroll-with-a-ticket-over-their-own-alpn.md),
[workload-auth.md](spec/workload-auth.md) §8.2). The server stores only each
ticket's hash in `<state-dir>/enrollment-tickets`, consumes it on first use,
and logs the ticket and credential ids an enrollment produced; revoke a
credential you do not recognize with `phux workload revoke`. Under `paired`
the QUIC listener completes a handshake without a client certificate so that
ALPN can answer, and closes any terminal connection without an enrolled one
before reading a byte of it.

`phux host add me@host` enrolls a client certificate over SSH and saves
owner-only key and certificate files in `<state-dir>/remotes/`. Its registry
entry supplies them on every dial without `PHUX_WORKLOAD_*` variables.
For renewal, expiry warnings, and failure-safe replacement, see
[client certificates and renewal](remote-access.md#client-certificates-and-renewal).

`phux pair revoke sha256:...` can also revoke an enrolled workload credential.
The CA key
(`<state-dir>/workload-ca.key`), the CA certificate, and the registry
(`<state-dir>/workload-keys`) are owner-only files, replaced under a lock by
atomic rename; `PHUX_WORKLOAD_CA`, `PHUX_WORKLOAD_CA_KEY`, and
`PHUX_WORKLOAD_KEYS` move them. Every directory above them must be
controlled by its owner (or root) as well: only the immediate directory is
checked, and the files are opened by path. A running server re-reads the registry when
it changes, so enrollment and revocation apply with no restart. The registry records a random instance id beside its generation,
and an admitted connection's credential carries both: only the pair names
one registry state. A malformed, insecure, or missing registry admits no
workload credential; the server never falls back to an older generation. A
transient read failure refuses only the connection it happened on. A
workload connection holds its registry scopes and the server enforces them
on every frame and command (next section). Revocation, expiry, and a ceiling
change that no longer contains a live connection's grant end that connection
too, within one registry poll (250 ms) or at the expiry instant: its input
leases and subscriptions are released, it receives
`ERROR { PERMISSION_DENIED }` then `DETACHED { AUTHORIZATION_REVOKED |
AUTHORIZATION_EXPIRED }`, and the transport closes. Its Terminals keep
running. A missing, insecure, or malformed registry ends every workload
connection once it has stayed so for five seconds, so a write caught mid-way
ends none; until a valid one is written, no workload connection is admitted
([workload-auth.md](spec/workload-auth.md) §7). `phux doctor` reports the
CA fingerprint and the registry generation.

#### Server identity and the CA pin

A server that provisions its TLS certificate (first remote listener, or the
`phux pair` before it) has the workload CA issue it, creating the CA first
if needed, and presents the chain: the leaf, then the CA
([ADR-0153](adr/0153-clients-pin-the-workload-ca.md),
[workload-auth.md](spec/workload-auth.md) §2). `phux pair` prints the CA
fingerprint beside the leaf fingerprint, `--json` reports it as
`ca_fingerprint`, and the connect link carries it as `ca`. Clients pin it:
`phux host add` and `--code` record it in `known-authorities` beside
`config.toml`, one line per server, keyed by its leaf pin. A client that
paired earlier and holds only a leaf pin records the CA on its first
connection to a server that presents one. Existing certificates are never
re-issued, so a server provisioned before this keeps its self-signed leaf,
presents no CA, and every pin a device already holds keeps working;
upgrading needs no re-pair.

A client pinning a CA refuses a server presenting another one before any
pairing token is sent, and does not retry:

```text
mini: the server's certificate authority changed: pinned sha256:..., presented sha256:....
Refusing to connect: a rotated authority and an impostor look the same from here.
If the host's operator rotated it (`phux workload authority --rotate`), re-pair: `phux host add mini`
```

Rotate only on purpose: after a CA key may have leaked, or to move a server
provisioned before ADR-0153 under its CA.

```sh
phux workload authority --rotate   # new CA + server certificate; prints both fingerprints
phux upgrade                       # present it (sessions survive)
```

Then re-pair every client: `phux host add NAME` on each machine that added
the host over ssh (it re-enrolls the workload certificate too), and a fresh
`phux pair --qr` for each phone. Workload certificates the old CA issued no
longer verify. The replaced CA, key, and server pair are kept beside the new
ones as `*.retired-<unix>`, owner-only. An operator-supplied certificate
(`PHUX_WS_TLS_CERT` / `PHUX_WS_TLS_KEY`) is never touched, and such a server
presents no CA unless its chain names one. Deleting a line from
`known-authorities` forgets that pin; the next connection learns it again.

#### Policy mode (`[policy] mode`)

`[policy] mode` in `config.toml` picks the server's authorization posture.
The server reads it once at start
([workload-auth.md](spec/workload-auth.md) §8):

- **Unset**, the default, keeps today's behaviour: every connection the
  server admits holds the owner's full grant. With a remote listener or
  relay connector configured, the server logs one warning at startup,
  because a pairing token then admits a consumer with command-execution
  authority. This transitional posture stays until the clients that reach
  such a server carry workload certificates: every entry point can carry
  one under `paired`, but a phone needs an app that enrolls, a relayed
  consumer a relay link that names the server's CA, and a browser a key it
  cannot hold yet. Naming a workload CA or registry location
  (`PHUX_WORKLOAD_CA`, `PHUX_WORKLOAD_CA_KEY`, `PHUX_WORKLOAD_KEYS`) with no
  mode refuses to start the server: set `mode = "paired"` to enforce that
  authority, or unset the variables. A registry at the default location,
  such as one `phux host add` enrolled into, refuses nothing.
- **`local`** admits the owner's Unix socket only, from the serving user's
  uid. A configured remote listener (`--listen`, `--quic`, `--webtransport`,
  their environment variables, or a `[[connector]]`) refuses to start the
  server, the overlay listener is never auto-bound, `phux attach --ssh`'s
  on-demand listener is refused, and any other connection is refused at
  HELLO.
- **`paired`** turns on the workload mTLS check above; `PHUX_WORKLOAD_MTLS`
  with no mode means the same. The owner's Unix socket keeps full
  authority. Every TLS connection must present an enrolled certificate and
  holds exactly its registry scopes, such as `observe,input@host` or `inventory@global`. A
  frame or command outside them is refused with `PERMISSION_DENIED`, and the
  connection stays up. Server-wide reads are filtered rather than refused:
  `phux ls`-style state, a server-wide event subscription, and an attach
  snapshot list only what the scopes cover, and the listener report needs
  `@global`. A refused HELLO ends with `DETACHED { AUTHENTICATION_FAILED }`.
  The scopes come from the registry at HELLO, never
  from a pairing token. A paired server refuses to start without usable
  workload authority material.

The `phux.whoami/v1` record carries the grant the asking connection holds,
and any connection may read its own.

Under `local` and `paired` the owner socket means the serving user's uid: a
peer running as another user, root included (`sudo phux ...`), is refused at
HELLO. Without a mode this is unchanged, and such a peer is admitted as
before. Registry scopes name `global`, `host`, or `host:<name>` only:
session and Terminal ids restart with the server, so `phux workload
add-key` refuses `group:` and `terminal:` selectors until stable identities
exist.

#### On-demand listeners (`phux attach --ssh`)

`phux attach --ssh HOST` opens a routable QUIC port without any of the setup
above ([ADR-0120](adr/0120-ssh-bootstrap-opens-a-listener-per-attach.md)).
Its trust rests on ssh. Only a process that can reach the server's Unix
socket, which on the host means the server's own user, can ask for one, and
`phux bootstrap` asks for it inside the operator's authenticated ssh session.
What that exposes, and for how long:

- One UDP port on the wildcard address per attach, serving TLS 1.3 with the
  server's persistent certificate. The client pins the fingerprint it
  received over ssh, so there is no trust-on-first-use window.
- A 256-bit token that admits only that listener. The server holds it in
  memory and never writes it to the token store; pairing tokens do not work
  on that listener, and its token works nowhere else. The token is outer
  admission only ([ADR-0116](adr/0116-workload-auth-is-mtls.md)).
- A bounded lifetime. The listener closes once no connection has been open
  through it for its linger (120 seconds by default, at most an hour), and
  never survives an upgrade or restart.
- A server that `phux bootstrap` starts is marked as started without a login
  shell, like a service unit, so its panes see the login profile's `PATH`.

Restrict the port with `--udp-ports MIN-MAX` when a firewall has to name it.

### Connecting from another network (overlay reachability)

The remote-consumer path authenticates and encrypts the link; it still
needs the client to **reach** the server's address. Overlay setup
(Tailscale, Headscale, WireGuard), `phux --remote`, and `phux pair` are
in [Remote access](./remote-access.md). An overlay IP is non-loopback, so
TLS and a bearer token engage automatically. Until you pair, the token
store is empty and the listener rejects every connection.

The auto-bound listener binds the **selected concrete address**, not
`0.0.0.0`: it does not listen on every interface. Routing and firewall rules
still determine who can reach that address, including when an explicit
public IP is selected. `PHUX_NO_AUTO_LISTEN=1` suppresses auto-listen;
`--listen` / `--quic` (or `PHUX_WS_ADDR` /
`PHUX_QUIC_ADDR`) still override the address. Only the default profile
auto-binds — a port is global to the host, so a `dev`-profile server
would otherwise race the installed one. Detection runs off-thread after
the UDS accept loop is live, so a wedged overlay CLI costs a late remote
listener (bounded at two seconds, after which detection falls back to
the CGNAT route heuristic unless `PHUX_TAILSCALE` is set), never a late
server.

One detector feeds `phux pair`, `phux doctor`, and the auto-bound remote
listener ([ADR-0081](adr/0081-overlay-auto-listen-and-one-command-pairing.md)).
For Defguard, raw WireGuard, or another routed network, set
`PHUX_OVERLAY_ADDRS=10.77.0.2,fd77::2` to the host's assigned tunnel
addresses. This comma-separated list replaces discovery entirely. It keeps
order and removes duplicates; the server auto-binds only the first address,
while pairing can select a matching family for an explicitly configured
wildcard listener. Use bare IP literals, not CIDRs, ports, interface names,
or bracketed/scoped IPv6. Loopback, unspecified, multicast, broadcast,
link-local, and IPv4-mapped IPv6 addresses are refused. An empty value
turns discovery off; any invalid item refuses the entire list with a
warning and no fallback to another network.

Set the value in both the server service environment and the pairing shell;
changing the shell does not reconfigure a running server. The addresses must
already belong to this host: phux does not install a VPN, assign addresses,
or verify peer routing. Startup binds once, with no automatic rebind after
VPN address changes; restart only when safe for the running sessions. This
is explicit address selection, not a private-prefix firewall: a public
unicast address is also accepted if deliberately configured. Keep phux's
TLS, credential, and policy checks plus the host/network firewall in place.
`PHUX_NO_AUTO_LISTEN`, profile/policy gates, and explicit listener overrides
still apply.

When `PHUX_OVERLAY_ADDRS` is unset, `PHUX_TAILSCALE` substitutes the CLI
(default: `tailscale` on PATH). Setting it also disables the CGNAT
route-probe fallback, including after the CLI's two-second deadline: once
you have named the overlay CLI, its answer is the whole answer. A command
that reports no address therefore turns overlay discovery off. See the
[Defguard federation masterplan](./architecture/defguard-federation.md) for
topology, rollout gates, and limits.

`phux doctor`'s remote-reachable check dials the running server's bound
wss address when that address is off-loopback, so a concrete `--listen`
is probeable without this detector. An unspecified `0.0.0.0`/`::` bind
still uses the detector to pick a host; with no overlay address the
check warns rather than passing. Test harnesses set `PHUX_NO_AUTO_LISTEN`
and `PHUX_TAILSCALE` so doctor does not dial the operator's tailnet
(phux-vlv1). On macOS a bound listener with a silent off-loopback probe
is the Application Firewall stealth-drop; see
[Remote access, Troubleshooting](./remote-access.md#troubleshooting).

### Running the reference relay

A self-hosted relay accepts an outbound QUIC tunnel per named server route.
Consumers select a route through TLS SNI and exchange frames through that
tunnel. **The relay terminates TLS on both connections and can read all phux
traffic, including input and terminal output.** Run it on a host you trust.
Setup: [Remote access, Path D](./remote-access.md#path-d-via-a-reference-relay).

The surface is two commands: `phux relay pair --route NAME` mints the
tunnel token and prints the fingerprint both legs pin; `phux relay run
--listen ADDR` runs the relay in the foreground. `--listen` has no
default; `--max-conns` (default 64) is the sole limiting knob. Pairing an
already-enrolled route replaces that route's token. Beyond that cap, one
source address may have at most 16 connections between its first packet
and its admission (the handshake, then a connector's auth preamble or a
consumer's first stream), plus 16 more that answered a QUIC Retry; past
that it is refused, so one address cannot occupy the handshake pool, and
spoofing an address cannot lock out the real one.

**State files.** Exactly three, at fixed paths in the phux state
directory (`$XDG_STATE_HOME/phux`, or `$HOME/.local/state/phux` when
unset) — siblings of the server's `remote-*` files:

- `relay-tokens` — one `<64-char hex token> <route>` line per enrolled
  route; `#` comments and blank lines are ignored; mode `0600`. The relay
  checks the file on every connection attempt (one `stat`) and re-reads it
  whenever it changed, so `phux relay pair` takes effect on a running
  relay and deleting a line revokes at the next handshake — no restart, no
  reload signal. A live tunnel survives its token's deletion until it
  drops or the relay restarts; restarting the relay is the immediate
  revocation path. A store owned by another account, or writable by
  others, is refused (the relay will not start, and a running one admits
  nobody until it is fixed); one others can read loads with a warning.
- `relay-cert.pem` / `relay-key.pem` — the relay's self-signed TLS pair,
  provisioned on first use and left untouched when both files exist, so
  the pinned fingerprint stays stable across restarts. Operator-supplied
  certificates work by placing PEM files at these paths. The key is
  written mode `0600`.

There are no path flags and no `PHUX_RELAY_*` environment variables.
Listing enrollments is reading the file; revoking is deleting a line;
re-pairing rotates. The connector token file on the server is re-read on
every dial, and must be owner-only and owned by the server's user.
Concurrent `phux relay pair` invocations are last-write-wins — run one at
a time.

**Private keys.** Every TLS private key a listener or the relay serves
with (`remote-key.pem`, `PHUX_WS_TLS_KEY`, `relay-key.pem`) is checked
when it is loaded. A key owned by another account (root excepted, for
keys an operator keeps under `/etc`), one other accounts can read, or one
anyone but its owner can write is refused with the `chmod` or `chown`
that fixes it. A group-readable key, such as one shared through an
`ssl-cert` group, is used with a warning.

Refusals are distinguishable at the consumer. An unknown or absent route
name fails during the TLS handshake itself — no phux bytes are exchanged.
An enrolled route with no live tunnel completes the handshake and then
closes with a distinct route-offline application code.

### Output mode for remote consumers

A remote phone link is high-latency and may be lossy. A remote consumer
SHOULD request `OutputMode::StateSync` at HELLO rather than the default
`OutputMode::Raw`: StateSync ships the minimum VT to move the consumer's
last-acked state to canonical per tick, coalescing floods and pacing
per-consumer RTT. Raw stays the default for local interactive peers,
where byte-faithful pass-through is lowest-latency on a fast link.

### Known limitations

- **Local transports are plaintext:** UDS and explicit loopback WebSocket
  carry plaintext. UDS relies on filesystem permissions; loopback WS is a
  development path. Routable WSS and QUIC listeners use TLS.
- **Scrollback unencrypted:** Terminal history is stored in the
  libghostty grid in RAM, unencrypted. A memory dump can recover it.
- **Encryption belongs to the transport:** phux frames have no
  independent per-command encryption. WSS and QUIC protect the complete
  stream with TLS.
- **No audit logging:** phux does not log which user accessed which
  terminal or when.
- **SSH-stdio delegates auth to SSH.** `ssh HOST phux stdio-bridge` is an
  ordinary local UDS client on the far host; there is no bearer token on
  that transport. Routable WebSocket, QUIC, and WebTransport listeners
  require a `phux pair` token over TLS.

### What you do get

- **Kernel-enforced permission boundary:** On Linux and macOS, the OS
  prevents other users from connecting to your socket.
- **No privilege escalation surface:** The server runs as your user (not
  setuid/setgid). A compromised terminal cannot elevate to other UIDs.
- **No eval RPC:** phux does not evaluate source text inside the server,
  but an authenticated consumer can spawn commands and drive shells with
  the server user's authority. Treat a remote token as command-execution
  access.
- **Process isolation via OS:** Each terminal's PTY is managed by the
  kernel; one terminal's PTY cannot directly access another terminal's
  memory or file descriptors.
