---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-09-12
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

A CLI verb whose **stdout reader hangs up** — `phux snapshot work | head
-8`, or quitting `less` mid-listing — is not an error. Every stdout write
in the `phux` binary goes through one helper that treats `BrokenPipe` as
a clean end of the pipeline: the process exits `0`, silently, running no
further output. Any other write failure (a full disk, `EIO`) is reported
as one stderr line with a failing status. The bin crate keeps clippy's
`print_stdout` lint armed so a new `println!` cannot reintroduce the
panic that used to end that pipeline.

A **malformed `config.toml` is fatal at server start**. `phux server`
loads the config exactly once; if the file exists but fails to load, the
server refuses to start (non-zero exit) and reports the config path, the
real loader error, and the remedy — `run: phux config check` — on **both**
stderr and the server log via `tracing::error` (the auto-spawn path nulls
stdio, so the log line is the durable trace there). A *missing* config
file is not an error: the server starts with the shipped defaults. The
server never silently reverts a broken config to defaults — a config the
user wrote either applies in full or stops the server with the reason.

## Runtime status

`phux status` is the one-glance answer to "is the server up, and what is
it doing": whether a server is listening at the socket and as which pid
(from the UDS peer credentials), since when (the socket file's bind time,
honest across a graceful upgrade), the protocol version it negotiates (a
real `HELLO`/`HELLO_OK` exchange), the summed attached-client count, one
line per session plus the satellite-terminal split, and both log paths. A
partial federation view is reported inline, never silently dropped.
`--json` emits a stable versioned document; with no server running the
human path prints the standard no-server diagnostic and `--json` answers
`{"running": false, ...}` on stdout — both exit 1. Everything it reports
is client-side sourcing over the existing wire; no protocol surface exists
for it.

Per-pane scrollback is a config knob, not an ops table: raising
`history-limit` on a wide grid does nothing, and raising `history-bytes`
costs attach latency. See [`CONFIG.md`](./CONFIG.md#scrollback).

## Logging and observability

Logs are both an operator surface and a leak surface;
[ADR-0028](adr/0028-runtime-log-control.md) owns that decision, and
this section is the home for the facts.

`tracing` is the structured logging substrate, bootstrapped in
`phux_server::telemetry`. Two entry points share one layer builder:

- **Server / foreground** (`phux server`, one-shot control verbs, any
  `--json` path) — `telemetry::init()`. Always logs human-or-JSON text to
  **stderr**; stdout is reserved for protocol/PTY traffic.
- **Client / TUI** (`phux attach`, naked `phux`, `phux new` without
  `--json`) — `telemetry::init_client()`. Logs to a **file only**: the
  attach loop owns the alt screen, so a stray log line corrupts the
  display.

Both fmt layers emit span-close timing (`FmtSpan::CLOSE`), so any
`#[instrument]` span reports elapsed duration at close.

### Performance observability

Every hop on the hot path records into an always-on, lock-free histogram
or counter that lives in the binary. There is nothing to enable. The
numbers a session was keeping while it felt slow are the numbers you read
afterwards.

```sh
phux perf              # lifetime table since the server started
phux perf --watch 1    # one interval per second: rates and per-second percentiles
phux perf --reset      # snapshot, then zero, so the next call is an interval
phux perf --json       # the raw PerfReport (schema_version inside)
```

The table is grouped by pipeline stage, top to bottom in the order bytes
move. The columns are `count` / `rate/s` for counters and `p50` `p90`
`p99` `max` for histograms; latencies are microseconds, sizes bytes.

| Group | What it measures | Healthy on a laptop over the local socket |
|---|---|---|
| `pty.read.size` | bytes per `read(2)` from a PTY. macOS caps this at 1024, so a burst is a spike of exactly-1024 reads | p99 at 1024 under a flood is the OS, not phux |
| `pty.reader.blocked` | reader thread parked because the actor's queue was full | 0; anything else means the actor is behind the child |
| `pty.queue_wait` | reader-to-actor queue delay | p99 under 1 ms |
| `pty.burst.bytes` / `pty.burst.chunks` | how many reads the actor coalesced into one parse and one frame | chunks above 1 under a flood is coalescing working |
| `pty.vt_apply` | libghostty parse time per burst | p99 under 2 ms at 16 KiB |
| `echo.server` | key or paste handed to the PTY writer until the next output from that pane, sampled only when the pane was quiet for the previous 100 ms; includes the child's own reaction | p50 under 1 ms for a shell prompt |
| `input.pty_write` | `write(2)` plus flush on the writer thread | p99 under 200 us |
| `tick.emit` / `tick.synth` / `tick.out_bytes` | state-sync fan-out: whole tick, per-consumer diff, per-consumer frame size | tick p99 under 5 ms; grows with consumers x rows |
| `consumer.mailbox_full` | ticks that skipped a consumer whose outbound queue was full | 0; a steady rate is a client that cannot drain |
| `consumer.ack_rtt` | emit to `FRAME_ACK` round trip per state-sync client | tracks the link: sub-ms local, tens of ms over QUIC |
| `pump.frames` / `pump.bytes` / `pump.frame.bytes` | raw broadcast fan-out volume and per-frame size | frame size near `pty.burst.bytes` |
| `pump.lagged` / `pump.gap_resync` | output pumps that fell behind (more than 256 frames, or a chunk older than 250 ms when dequeued), and the resyncs that cost; each resync re-bootstraps only the pump that fell behind | 0 |
| `wire.write` / `wire.write.bytes` / `wire.bytes_out` | coalesced socket writes per client | p99 under 500 us on UDS |
| `cmd.handle` / `attach.handle` | control-plane latency | attach p99 under 100 ms with a warm history |
| `proc.*` | clients, panes, sessions (gauges) and, in the header, CPU split, peak RSS, context switches | idle CPU under 1 percent with agents running in panes |

The client keeps its own table. When an attach ends it writes one
`session perf:` line to its log (`phux logs --client`) with the echo
round trip (keystroke out to first output frame back for that pane),
`vt_apply` and `paint.full` percentiles, frame counts, pacer waits, and
stdout drops. `PHUX_RENDER_PROF=1` still emits the per-second
`render_prof` line, now with `echo_p50_us` / `echo_p99_us` beside the
counters. Degradations that used to log at debug — a full consumer
mailbox, a dropped stdout backlog — now warn at most once per ten seconds
with a `suppressed` count, so they are visible at the default filter
without flooding it; a broadcast lag already warned and now also counts
(`pump.lagged`).

For a reproducible number rather than a live one, `just perf-echo` runs
the byte-level echo probe against an isolated server at a chosen size
with a flooding sibling pane, and `just profile` records a CPU profile of
the `profiling` build (symbols kept) with samply.

### Environment knobs

| Variable | Effect |
|---|---|
| `RUST_LOG` | Filter directives. Default `phux=info,warn`. |
| `PHUX_LOG=<path>` | Write logs to `<path>` via non-blocking file writer. Server tees to this file *in addition to* stderr; client writes here *instead of* its per-pid default. Parent directory created if missing. |
| `PHUX_LOG_FORMAT=text\|json` | `text` (default): human single-line layer. `json`: one JSON object per line for `jq`/`grep`. Applies to both stderr and file sinks. |
| `PHUX_RENDER_PROF=1` | Client only. Emits one `render_prof` INFO line per second carrying the attach loop's paint counters: `frames` (inbound `RESOURCE_OUTPUT` frames applied), `paints` (composited frames emitted), `skipped` (paints withheld by coalescing or the frame pacer), `bar_composes` (runs of the status-bar widget pipeline), `layouts` (pane tilings that missed the layout cache), `paced_replies` (frames admitted because they answer the user's input rather than arriving unsolicited) and `paced_waits` (frames the pacer held back for its window), and `flushes` / `bytes` (what reached the off-loop stdout writer). The `paced_replies` / `paced_waits` pair is how an input-latency regression in the paint scheduler is told apart from load on the box. Free when unset. |
| `PHUX_FRAME_INTERVAL_MS=<ms>` | Client only. Minimum interval between composited frames; default `16` (one frame at 60Hz). The first frame after any lull always paints immediately, and output from the pane the user last acted on is never paced while its reply grace is open, so this only bounds how often a sustained *unsolicited* output stream repaints. `0` disables pacing entirely. |
| `PHUX_INPUT_GRACE_MS=<ms>` | Client only. Pins the *input reply grace*: how long after the user types, clicks, or pastes that pane's output still counts as a reply and bypasses frame pacing. Unset, the grace is measured — `max(20ms, 2 x the observed input-to-output latency)`, capped at 250ms — so a session over QUIC to a distant server sizes it to that link instead of to a unix socket's microseconds. The grace is keyed to the pane the input was routed to, so typing in one pane never lifts pacing for a flood in another, and pointer motion does not arm it (a drag would otherwise refresh it continuously). `0` disables the grace, so every frame is paced. |
| `PHUX_TTY_READINESS=0` | Client only. Read the outer terminal through tokio's blocking-pool stdin instead of on reactor readiness. The readiness path opens its own non-blocking handle to the terminal and wakes the attach loop straight off `kqueue`/`epoll`, saving a thread handoff per keystroke; it falls back on its own when the platform will not poll a terminal fd. Set this when a terminal that *is* pollable behaves badly on it — the fallback is the pre-`0.23` behaviour. |

The **canonical server log** is `$XDG_STATE_HOME/phux/server.log` (falls
back to `$HOME/.local/state/phux/`). Every spawn path writes the same
file: the auto-spawned daemon redirects its stderr there, and the
`phux service` unit points its log capture at it. Both resolve the path
through `phux_server::telemetry::server_log_path`, so the writers and
every reader (`phux logs`, `phux service logs`) can never disagree about
where it is.

The **client default log path** (when `PHUX_LOG` is unset) is
`$XDG_STATE_HOME/phux/client-<pid>.log` (falls back to
`$HOME/.local/state/phux/`). The pid scope keeps concurrent clients from
interleaving. Level defaults to `phux=info,warn`, so crashes and warnings
are always captured without flooding the file.

The non-blocking file writer offloads I/O to a background thread; its
`WorkerGuard` is held for the lifetime of `main` and flushes when it is
dropped on a normal exit. An `abort` skips that Drop, which is why the
panic hook writes its crash record synchronously as well (see "Crash
capture").

### Sensitive data in logs

Log sinks are created with mode `0o600` (owner-only) on Unix so another
user on a shared box cannot read them. Input atoms are **self-narrating
and redaction-safe**: `KeyEvent` and `PasteEvent` have hand-written
`Debug` impls (and `InputEvent::narrate`) that report only structural
facts — action, physical key, modifiers, payload *lengths* — and never
the typed key text or pasted bytes. A `trace!(?input, …)` therefore
records that a keystroke or paste happened, with its shape, without
spilling the secret it carried.

### Remote WebSocket pairing admissions

The default log keeps remote pairing diagnosis visible without making
request material visible. Its decision tree is:

- Every peer-caused TLS, pairing-authentication, or WebSocket-upgrade
  rejection produces one `DEBUG` event naming its safe `stage` and
  `source_ip`. A listener-wide limiter also emits an immediate `WARN`,
  then at most one `WARN` per 60 seconds while further rejections arrive.
  A later warning carries the latest safe stage/source IP and
  `suppressed_count` since the previous warning. The limiter has one
  saturating counter, not a per-IP map, so hostile source churn cannot
  grow memory or flood the default log. These handled events never fall
  through to the accept loop's default `ERROR`, so they are not
  duplicated.
- Listener and resource failures the WebSocket listener did not create
  (such as TCP accept exhaustion), and accept errors from listeners using
  the default disposition, retain the shared accept loop's single `ERROR`
  event.
- Peer rejection errors are a concrete safe type recognized without
  parsing error text. They retain stage diagnosis but exclude the
  ephemeral port and discard the underlying TLS/HTTP/WebSocket error so
  URIs, headers, certificates, and tokens cannot be formatted
  accidentally. Missing, malformed, unknown, and revoked tokens all use
  the same pairing-authentication text and the same generic HTTP 401
  response.
- A successfully token-authenticated WebSocket admission produces one
  `INFO` event with only `transport=ws`, `source_ip`, and
  `credential_id`. The stable, non-secret credential ID correlates
  reconnects and rotated generations; it is not derived from or equal to
  the bearer token.
- Anonymous loopback WebSocket admissions and admissions on other
  transports retain the shared `DEBUG` connection event; they gain no
  default-visible identity event.

These are connection diagnostics, not a durable access or per-operation
audit log. `PeerIdentity` is never logged wholesale.

### Crash capture

Panics are durable on both sides. The **client** panic hook logs the
panic message plus a captured `std::backtrace::Backtrace` to its file
sink *before* it restores the terminal (survives even though the default
hook's stderr backtrace would vanish into the dead alt screen). The
**server** panic hook logs task/actor panics with their backtrace through
`tracing`, so a daemonized server's crash lands in the log file. Both
honor `RUST_BACKTRACE` for trace verbosity.

The server hook is armed by the **long-running daemons only** — `phux
server` and `phux relay run` install it on entry, not `telemetry::init`.
A one-shot CLI verb shares that subscriber but keeps the default panic
hook: when it was armed process-wide, a CLI that died reported itself as
a `server panic`, sending triage after a server that never faltered.
Nothing in a one-shot verb needs a durable crash record, because the
operator is reading its stderr as it happens.

The server hook's `tracing` event goes into the non-blocking appender's
queue, and the release profile is `panic = "abort"` — no unwind, so the
`WorkerGuard`'s flush-on-Drop never runs and a queued record would die in
the worker's buffer. The hook therefore also appends the same facts to
`PHUX_LOG` **synchronously** (mode `0o600`, like every other sink) before
returning. Under unwind the queued copy may land too, so a debug build
can show the panic twice. Release builds keep symbols and line tables
(`[profile.release]` sets `debug = "line-tables-only"`, `strip =
"none"`), so the logged backtrace resolves to function names and source
lines rather than bare addresses.

### Blast radius of a panic

There is **one server process per user**
([ADR-0003](adr/0003-server-process-model.md)) on one current-thread
runtime, and the release profile aborts on panic. A panic anywhere in the
daemon ends every session, window, and pane for that user at once; it is
not contained to the pane actor that raised it. Planned restart (`phux
update` / `phux upgrade`) preserves sessions by passing the listening fd
to the new image. An unplanned death has no equivalent — the crash record
is what the operator gets. `panic = "abort"` is a deliberate choice (no
half-unwound actor state, smaller binary, no landing pads); nobody should
read "durable panics" as "contained panics".

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

`phux logs` is the discovery verb. Bare invocation prints the inventory
— the canonical server log, the per-pid client logs (newest first), and
the state dir that holds them — with existence, size, and age; a file
that does not exist yet is reported as "not created yet", never as an
error. `phux logs --server` tails the server log and `phux logs --client`
the newest client log (`--pid PID` picks a specific one); `-f` follows
and `-n NUM` sets the tail length. `phux logs --json` emits the inventory
as a stable `schema_version` 1 document on stdout. `phux service logs` is
the same tail over the same server log, kept for symmetry with the other
`service` verbs.

There is no Prometheus/OpenTelemetry exporter, or runtime per-target
log-level control. Use `phux status` and `phux ls --json` for the running
view and the environment-controlled tracing sinks above for diagnosis.
There is no access audit log.

## Voice

`TRANSCRIBE` runs the argv in `[voice].transcriber` on an uploaded clip
and pastes stdout into the pane. Without `[voice]` the command is
refused. Schema and defaults:
[`docs/reference/config.md`](./reference/config.md).

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

Notes that matter in practice:

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

phux is developed on the same machines it is used on, so a development
build must not be able to touch the installed build's sessions. Every
phux process resolves a **profile** that scopes where it looks
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

Detection is automatic — a binary under a Cargo `target/` directory, or
one built with `debug_assertions`, is a development build — because a
variable a developer has to remember is not isolation. Set
`PHUX_PROFILE` explicitly to run more than two instances, e.g. one per
agent worktree.

The consequence to expect: **a `cargo run` build will not show your
installed phux's sessions.** That is the point, and `phux doctor`
reports a non-default profile as a warning so it is never a mystery:

```
warn instance  profile dev (this is a development build …); state …/phux-dev
```

## Restart policy and crash-loop visibility

The generated unit restarts the server on **abnormal exit only**,
throttled to one start per 30s:

| | launchd | systemd |
|---|---|---|
| restart when | `KeepAlive{SuccessfulExit:false}` | `Restart=on-failure` |
| throttle | `ThrottleInterval 30` | `RestartSec=30s` |
| give up after | *(no such knob)* | `StartLimitBurst 5` / `StartLimitIntervalSec 180s` |

Throttling is not giving up: `ThrottleInterval` and `RestartSec` set a
*minimum spacing* between starts, not a limit on how many. systemd's
start limit supplies the missing bound, sized so five throttled starts
fit inside the window — below that the limit is unreachable and the unit
retries forever. launchd has no equivalent, which is why
`phux service install` refuses up front when a server already holds the
socket rather than relying on the supervisor to notice a start that can
never succeed.

Two consequences worth knowing:

- **A deliberately stopped server stays stopped.** `phux kill --server`
  asks the server to stop over the wire, so it exits *cleanly* and the
  supervisor leaves it alone. Earlier units used `KeepAlive: true`, which
  restarts on *every* exit — a server could not be stopped at all. A
  server killed by a signal still counts as an abnormal exit under
  launchd and comes back, which is why the stop is a command rather than
  a `kill(1)`. Note the next `phux attach`/`phux new` auto-spawns a fresh
  server: this stops the current one, it does not disable phux.
- **A crash-loop is visible.** Every server start appends a record to
  `$XDG_STATE_HOME/phux/server-starts.log`, and `phux doctor` *fails* the
  `server-health` check when the server has started 5+ times in an hour:

  ```
  fail server-health  the server started 9 times in the last 60 minutes — it is crash-looping
                      -> something is killing the server on startup; the reason is in …/server.log
  ```

  A supervised server that dies and restarts otherwise looks identical to
  one that never fell over — the socket answers either way. Counting the
  restarts is what makes the difference legible.

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

phux detects this. Each server records its version in the start history,
so a client can see a mismatch the wire handshake cannot show it (that
negotiates the *protocol* version, not the build). On attach, the
handoff happens automatically:

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

`phux service install` refuses while a server holds the socket, because
the supervised process would fail to bind on every start and retry
forever. `phux service install --adopt` is the way past that refusal
without stopping anything:

```
$ phux service install --adopt
phux service armed (nothing was stopped).
  unit    ~/Library/LaunchAgents/com.phux.server.plist
  panes   untouched — the running server was not signalled
```

The unit is written from your flags exactly as a plain install writes it,
and then **armed** rather than loaded — the file is on disk and the init
system is committed to it, but nothing has been started. Supervision
takes over at whichever comes first: the next login or reboot, or the
first `phux` command after the running server exits, which starts the
supervised server instead of auto-spawning an unsupervised one.

What `--adopt` deliberately does **not** do is put the currently running
process under restart supervision. Nothing can: launchd has no way to
place an existing process under a job, and systemd's scope units track
processes without restarting them. Adoption therefore transfers the
supervision, not the process — the panes survive because the running
server is never touched, not because they are handed over. `phux service
status` reports `state armed` while an adoption is pending, and
`phux service uninstall` cancels it.

Over a socket with nothing listening, `--adopt` is an ordinary install.
The flag means "never stop a running server to install", so it is always
safe to pass.

### Scheduling class

The server is the keystroke path between the user and every pane, so it
runs in the interactive scheduling class, not the batch one. On macOS
every thread that carries input or its echo — the runtime thread, each
PTY reader and writer, the input lane, the client's attach loop and
stdout writer — requests `QOS_CLASS_USER_INTERACTIVE` for itself at start
(no privilege needed), and the launchd unit `phux service install`
writes declares `ProcessType` `Interactive`. `phux perf` reports the
result as `proc.sched_interactive` (`1` granted, `0` not). Until
2026-09-02 the unit said `Background`, which asked launchd to throttle
exactly this process; under a full-CPU load (a cargo build, a fleet of
agents) that turned a 0.5 ms keystroke echo p99 into 15-60 ms with the
server itself using well under one percent of a core. `phux service
reconcile` (run automatically after an update) rewrites an installed
`Background` unit to `Interactive`; the change takes effect when launchd
next starts the server. Linux has no unprivileged equivalent (lowering
`nice` needs `CAP_SYS_NICE`), so the request is a no-op there and the
gauge reads `0`.

## Service-managed pane environment

`phux service install` runs the server under launchd or systemd, both of
which start their unit with a minimal environment: no login shell ever
ran, so `PATH` additions a Homebrew or Nix installer put in
`~/.zprofile` / `~/.profile` never take effect. Left alone, every pane
the server spawns would inherit that minimal `PATH` — `nvim` and `brew`
reporting "command not found" even though an ordinary interactive shell
on the same machine has them.

**The fix is conditional, not a blanket default.** `phux service
install` stamps `PHUX_SERVICE_MANAGED=1` into the unit it generates (both
the launchd `EnvironmentVariables` dict and the systemd `Environment=`
lines). `phux server` checks for that marker at its own startup and, only
when present, spawns every command-less pane's shell in its platform
**login** mode instead of a plain interactive shell:

| shell        | login flag |
|--------------|------------|
| `bash`       | `-l`       |
| `zsh`        | `-l`       |
| `fish`       | `--login`  |
| `sh`         | `-l`       |

A `defaults.shell` naming anything else gets no login flag at all, even
under a service-managed server — an unrecognized program has unknown
flag semantics, and a pane that fails to spawn is a worse outcome than
one whose profile did not run.

**A hand-started server is unaffected.** `phux server` run directly from
a terminal, or auto-spawned by a bare `phux`/`phux new`, never carries
the marker and keeps spawning plain, non-login panes exactly as before —
that environment is already profile-initialized, and re-sourcing it a
second time is not idempotent for every setup (`nvm`/`rbenv`/`direnv`
guards misfiring, not just PATH duplication). This is why the marker is
something the installer writes and the server reads back, rather than a
guess from environment shape.

**Applying the fix to an already-installed service** requires rerunning
`phux service install`: the marker is only in units generated after this
change, and the server reads it once, at its own startup.

`phux service install` also never freezes the installing shell's own
transient `PATH` into the generated unit — running the installer from
inside `nix develop` or direnv leaves the unit exactly as portable as
running it from a plain shell. The init system's own `PATH` reaches the
server unmodified; login-shell treatment is how a *pane* recovers the
profile's `PATH`, not a baked-in snapshot of the installer's.

## Workspace continuity and update survival

phux has two different continuity mechanisms. They are intentionally
separate:

- **Restart restore:** `phux workspace save` writes a typed JSON archive
  of the running workspace. `phux workspace restore ARCHIVE` reads that
  archive and creates any missing session names on a running server. Each
  restored session starts a fresh PTY process: the archived `command` is
  used when present; otherwise phux starts the default shell in the
  archived cwd when available. This is a restart/recreate path, not a
  live handoff path. Restore currently recreates missing **sessions and
  seed processes** only; it does not replay the archived split tree.
- **Live update handoff:** `phux upgrade` keeps existing PTYs alive
  across a server binary re-exec. `phux update` is the user-facing verb
  built on that handoff: it resolves the published release, verifies the
  `.sha256` sidecar, replaces the binaries atomically, then calls
  `phux upgrade`. Default is the latest `vX.Y.Z`; `--channel next` follows
  green `main`. It writes only to installs it maintains — a Homebrew,
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
[Remote access](./remote-access.md): `phux host enroll --role satellite HOST`
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
  token or certificate pin on this transport. The bridge labels the
  connection `ssh-stdio` in `phux whoami`, and that label grants nothing:
  its trust is exactly the UDS peer's. Treat `ssh_client` as a label the
  connecting side reported, not a verified address.

### Remote consumer trust model (opt-in)

A remote consumer can attach over the network without an SSH tunnel,
behind TLS plus a bearer pairing token
([ADR-0031](adr/0031-remote-consumer-auth-and-encryption.md)). The bind
address is the toggle:

- **Loopback address → plaintext, unauthenticated.** The historical
  browser-client dev path; zero config.
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
never persisted. Pairing, rotation, and revocation take effect at the
next connection attempt, with no restart. An already-established session
is not re-authorized and survives revocation until it drops. Rotation
defaults to a 300-second overlap. Certificate lifecycle is an operator
responsibility, like socket permissions: with a self-signed certificate,
verifying the `phux pair` fingerprint on the device's first connect is
what closes the trust-on-first-use MITM window.

### Connecting from another network (overlay reachability)

The remote-consumer path authenticates and encrypts the link; it still
needs the client to **reach** the server's address. Overlay setup
(Tailscale, Headscale, WireGuard), `phux --remote`, and `phux pair` are
in [Remote access](./remote-access.md). An overlay IP is non-loopback, so
TLS and a bearer token engage automatically. Until you pair, the token
store is empty and the listener rejects every connection.

The auto-bound listener binds the **detected overlay address**, not
`0.0.0.0`, so it is invisible off the overlay. `PHUX_NO_AUTO_LISTEN=1`
suppresses it; `--listen` / `--quic` (or `PHUX_WS_ADDR` /
`PHUX_QUIC_ADDR`) still override the address. Only the default profile
auto-binds — a port is global to the host, so a `dev`-profile server
would otherwise race the installed one. Detection runs off-thread after
the UDS accept loop is live, so a wedged overlay CLI costs a late remote
listener, never a late server.

### Running the reference relay

A server behind NAT can dial out to a self-hosted relay and hold one QUIC
tunnel per named route; remote consumers dial the relay, name the route
via TLS SNI, and are spliced onto that tunnel byte for byte. **The relay
terminates TLS on both legs, so it sees every phux frame in plaintext**
— every keystroke and every rendered cell crosses the relay decrypted.
The mitigation is not a protocol feature; it is self-hosting. Run the
relay yourself, on a host you trust. Setup is [Remote access, Path
D](./remote-access.md#path-d-via-a-reference-relay).

The surface is two commands: `phux relay pair --route NAME` mints the
tunnel token and prints the fingerprint both legs pin; `phux relay run
--listen ADDR` runs the relay in the foreground. `--listen` has no
default; `--max-conns` (default 64) is the sole limiting knob. Pairing an
already-enrolled route replaces that route's token.

**State files.** Exactly three, at fixed paths in the phux state
directory (`$XDG_STATE_HOME/phux`, or `$HOME/.local/state/phux` when
unset) — siblings of the server's `remote-*` files:

- `relay-tokens` — one `<64-char hex token> <route>` line per enrolled
  route; `#` comments and blank lines are ignored; mode `0600`. The relay
  re-reads this file on every connection attempt, so `phux relay pair`
  takes effect on a running relay and deleting a line revokes at the next
  handshake — no restart, no reload signal. A live tunnel survives its
  token's deletion until it drops or the relay restarts; restarting the
  relay is the immediate revocation path.
- `relay-cert.pem` / `relay-key.pem` — the relay's self-signed TLS pair,
  provisioned on first use and left untouched when both files exist, so
  the pinned fingerprint stays stable across restarts. Operator-supplied
  certificates work by placing PEM files at these paths. The key is
  written mode `0600`.

There are no path flags and no `PHUX_RELAY_*` environment variables.
Listing enrollments is reading the file; revoking is deleting a line;
re-pairing rotates. The connector token file on the server is re-read on
every dial. Concurrent `phux relay pair` invocations are last-write-wins
— run one at a time.

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
