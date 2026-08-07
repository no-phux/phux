---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-08-07
---

# Operations

**TL;DR.** How phux behaves at its operational seams: error translation at
the wire boundary, structured and redaction-safe logging, workspace continuity,
remote-listener authentication, running the reference relay, and the exact
trust boundary. `phux status` reports the running server (pid, up-since,
protocol, clients, sessions, log paths); phux has no durable access audit log
today.

---

## Error model

Library and binary boundaries use typed Rust errors appropriate to their
module; there is no single workspace-wide error enum. Errors that cross the
IPC boundary translate to `ERROR` messages with a stable `ErrorCode` and a
human-readable message. [`spec/proto.md`](./spec/proto.md) owns that wire shape
and the code catalog.

A CLI verb whose **stdout reader hangs up** — `phux snapshot work | head -8`,
or quitting `less` mid-listing — is not an error. Every stdout write in the
`phux` binary goes through one helper that treats `BrokenPipe` as a clean end
of the pipeline: the process exits `0`, silently, running no further output.
Any other write failure (a full disk, `EIO`) is reported as one stderr line
with a failing status. The bin crate keeps clippy's `print_stdout` lint armed
so a new `println!` cannot reintroduce the panic that used to end that
pipeline.

A **malformed `config.toml` is fatal at server start**. `phux server` loads
the config exactly once; if the file exists but fails to load, the server
refuses to start (non-zero exit) and reports the config path, the real loader
error, and the remedy — `run: phux config check` — on **both** stderr and the
server log via `tracing::error` (the auto-spawn path nulls stdio, so the log
line is the durable trace there). A *missing* config file is not an error: the
server starts with the shipped defaults. The server never silently reverts a
broken config to defaults — a config the user wrote either applies in full or
stops the server with the reason.

## Runtime status

`phux status` is the one-glance answer to "is the server up, and what is it
doing": whether a server is listening at the socket and as which pid (from
the UDS peer credentials), since when (the socket file's bind time, honest
across a graceful upgrade), the protocol version it negotiates (a real
`HELLO`/`HELLO_OK` exchange), the summed attached-client count, one line per
session plus the satellite-terminal split, and both log paths. A partial
federation view is reported inline, never silently dropped. `--json` emits a
stable versioned document; with no server running the human path prints the
standard no-server diagnostic and `--json` answers `{"running": false, ...}`
on stdout — both exit 1. Everything it reports is client-side sourcing over
the existing wire; no protocol surface exists for it.

## Logging and observability

Logs are both an operator surface and a leak surface; [ADR-0028](../ADR/0028-runtime-log-control.md) owns that decision and its slicing, and this section is the home for the facts.

`tracing` is the structured logging substrate, bootstrapped in `phux_server::telemetry`. Two entry points share one layer builder:

- **Server / foreground** (`phux server`, one-shot control verbs, any `--json` path) — `telemetry::init()`. Always logs human-or-JSON text to **stderr**; stdout is reserved for protocol/PTY traffic.
- **Client / TUI** (`phux attach`, naked `phux`, `phux new` without `--json`) — `telemetry::init_client()`. Logs to a **file only**: the attach loop owns the alt screen, so a stray log line corrupts the display.

Both fmt layers emit span-close timing (`FmtSpan::CLOSE`), so any `#[instrument]` span reports elapsed duration at close.

### Environment knobs

| Variable | Effect |
|---|---|
| `RUST_LOG` | Filter directives. Default `phux=info,warn`. |
| `PHUX_LOG=<path>` | Write logs to `<path>` via non-blocking file writer. Server tees to this file *in addition to* stderr; client writes here *instead of* its per-pid default. Parent directory created if missing. |
| `PHUX_LOG_FORMAT=text\|json` | `text` (default): human single-line layer. `json`: one JSON object per line for `jq`/`grep`. Applies to both stderr and file sinks. |

The **canonical server log** is `$XDG_STATE_HOME/phux/server.log` (falls back
to `$HOME/.local/state/phux/`). Every spawn path writes the same file: the
auto-spawned daemon redirects its stderr there, and the `phux service` unit
points its log capture at it. Both resolve the path through
`phux_server::telemetry::server_log_path`, so the writers and every reader
(`phux logs`, `phux service logs`) can never disagree about where it is.

The **client default log path** (when `PHUX_LOG` is unset) is `$XDG_STATE_HOME/phux/client-<pid>.log` (falls back to `$HOME/.local/state/phux/`). The pid scope keeps concurrent clients from interleaving. Level defaults to `phux=info,warn`, so crashes and warnings are always captured without flooding the file.

The non-blocking file writer offloads I/O to a background thread; its `WorkerGuard` is held for the lifetime of `main` and flushes on exit.

### Sensitive data in logs

Log sinks are created with mode `0o600` (owner-only) on Unix so another user on a shared box cannot read them ([ADR-0028](../ADR/0028-runtime-log-control.md)). Input atoms are **self-narrating and redaction-safe**: `KeyEvent` and `PasteEvent` have hand-written `Debug` impls (and `InputEvent::narrate`) that report only structural facts — action, physical key, modifiers, payload *lengths* — and never the typed key text or pasted bytes. A `trace!(?input, …)` therefore records that a keystroke or paste happened, with its shape, without spilling the secret it carried.

### Crash capture

Panics are durable on both sides. The **client** panic hook logs the panic message plus a captured `std::backtrace::Backtrace` to its file sink *before* it restores the terminal (survives even though the default hook's stderr backtrace would vanish into the dead alt screen). The **server** panic hook logs task/actor panics with their backtrace through `tracing`, so a daemonized server's crash lands in the log file. Both honor `RUST_BACKTRACE` for trace verbosity.

The server hook is armed by the **long-running daemons only** — `phux server` and `phux relay run` install it on entry, not `telemetry::init`. A one-shot CLI verb shares that subscriber but keeps the default panic hook: when it was armed process-wide, a CLI that died reported itself as a `server panic`, sending triage after a server that never faltered. Nothing in a one-shot verb needs a durable crash record, because the operator is reading its stderr as it happens.

### Reading a trace to localize lag

The hot paths carry `tracing` spans whose `CLOSE` event reports the span's duration (`time.busy`/`time.idle`), so a captured session shows where time went before a stall. The per-frame and per-tick spans are at **debug**, so the default `phux=info` filter leaves them off and effectively free; raise the level only while diagnosing.

```sh
PHUX_LOG=/tmp/phux.jsonl PHUX_LOG_FORMAT=json RUST_LOG=phux=debug phux ...
# headless repro that exercises the same server paths:
PHUX_LOG=/tmp/phux.jsonl PHUX_LOG_FORMAT=json RUST_LOG=phux=debug \
  cargo run -p phux-server --example e2e-repro
```

Two spans carry most of the signal. On the server, `synthesize_against_reference` (fields `changed_row_count`, `out_bytes`) is the per-tick CPU cost of diffing engine state for one consumer. On the client, `handle_server_frame` (grep `kind=terminal_output`) is the per-frame apply-and-paint cost; its children `vt_apply` (libghostty parse) and `paint_trigger` (render) let you attribute a client stall to parse versus paint by comparing their `time.busy`. Narrow a JSON capture to timed events with `jq -c 'select(.fields.message=="close")'`. Finer per-PTY-chunk and per-frame-emit detail is at **trace**; a wedged or leaked consumer shows as `consumer mailbox full` / `consumer mailbox closed` at debug.

### Finding and tailing the logs

`phux logs` is the discovery verb. Bare invocation prints the inventory —
the canonical server log, the per-pid client logs (newest first), and the
state dir that holds them — with existence, size, and age; a file that
does not exist yet is reported as "not created yet", never as an error.
`phux logs --server` tails the server log and `phux logs --client` the
newest client log (`--pid PID` picks a specific one); `-f` follows and
`-n NUM` sets the tail length. `phux logs --json` emits the inventory as
a stable `schema_version` 1 document on stdout. `phux service logs` is
the same tail over the same server log, kept for symmetry with the other
`service` verbs.

There is still no `phux server status`, Prometheus/OpenTelemetry exporter,
or runtime per-target log-level control. Use `phux ls --json` for the
published session/pane view and the environment-controlled tracing sinks
above for diagnosis. [ADR-0028](../ADR/0028-runtime-log-control.md)
records the remaining operator surface.

## Agent-state detection

To make adoption automatic rather than relying on every operator to remember a
special launch command, run this once on each box:

```sh
phux agent install-claude
```

After a new shell starts, plain interactive `claude` creates and attaches a
phux session when invoked outside one, or runs in the current pane when already
inside. The shim injects additional Claude hook settings for lifecycle
publication: prompts set `working`, permissions/notifications set `blocked`
and emit `phux ask`, stops set `done`, and session exit clears the record.
Administrative and noninteractive Claude commands bypass the shim. Remove the
owned files and the marked shell-rc block with `phux agent uninstall-claude`.

The server derives each pane's `phux.agent/v1` record on a timer
([ADR-0046](../ADR/0046-server-side-agent-state-detection.md)). What it reads,
exactly, and nothing else:

- **The pane's own PTY.** Its foreground process group id, and that process's
  `argv` (`/proc/<pid>/cmdline` on Linux, a `sysctl` on macOS; unavailable
  elsewhere, where the detector simply never identifies an agent). This is used
  only to answer "which agent binary is running here", and only for terminals
  this server owns.
- **That terminal's OSC title and its live viewport rows.** Both are already in
  the server's own engine state; the detector reads them, matches them against
  its rule manifests, and derives a state word.

Nothing leaves the process: no network call, no subprocess, no file write. Screen
content is **not** logged — the detector logs its derived state transitions at
`debug` and its rule-match bookkeeping at `trace`, never the matched text.

**Kill switch.** `PHUX_AGENT_DETECT=0` in the server's environment loads an empty
rule set, so no detector is constructed and no pane is scanned. Consumers fall
back to their pre-ADR-0046 title heuristics.

**Rule manifests.** Built-in manifests for Claude Code, Codex, OpenCode, Pi, and
OMP are compiled into the binary. Additional or replacement manifests are read
from `$PHUX_AGENT_RULES_DIR` (default
`$XDG_CONFIG_HOME/phux/agent-rules`), one TOML file per agent kind; a manifest
replaces the built-in of the same `kind`. Manifests are loaded and their patterns
compiled **once**, on first use. A manifest that fails to parse, or that carries
an invalid pattern, is logged at `warn` and **dropped whole** — never partially
applied — so a bad rule file degrades detection for that agent kind rather than
wedging a pane. Grep the log for the manifest's path to find it.

**When it is wrong.** Detection is level-triggered and fail-safe: a pane whose
screen matches no rule reads `idle`, never `blocked`, and the next tick
re-derives from scratch, so a wrong value corrects itself rather than sticking.
A stale manifest therefore shows up as agents that never leave `idle` — not as a
sidebar stuck on red.

## Server lifetime

A phux server stops on exactly three conditions. The first two are the
defaults; the third has to be asked for.

1. **Signalled.** Ctrl-C on a foreground `phux server`, or any signal that
   ends the process.
2. **Last pane reaped**, once at least one client has been served. When a
   pane's process exits the runtime reaps it, cascading to its window and
   session; an empty server exits. The "served a client" guard exists so a
   freshly auto-spawned server whose seed pane dies before anyone connects
   stays up long enough for the launching `phux` to repopulate it.
3. **Unattended past `--exit-after-idle SECS`**, if that flag was passed.

`phux server --exit-after-idle SECS` is for **ephemeral** servers: a test
harness or CI job that bootstraps a private server per run on a temp socket
and cannot guarantee its own cleanup step will execute. Such a server exits
once no client has been *connected* for `SECS` — live panes and all. Both
"nobody ever connected" (the clock runs from startup) and "the last client
left" are covered.

Notes that matter in practice:

- **Connected, not attached.** One-shot control verbs (`phux ls`,
  `phux send-keys`, `phux new --json`) count: each connect postpones the
  exit. A scripted harness that never attaches is safe.
- **Quiet does not mean idle.** An open connection pins the server alive no
  matter how long it sends nothing, so an attached human is never reaped.
- **The exit is the graceful one.** It runs the same teardown as Ctrl-C:
  each pane's process group is SIGHUPed, given a grace period, and the child
  is reaped; the socket is unlinked.
- **It survives `phux upgrade`.** The lifetime is re-passed on the re-exec,
  so upgrading an ephemeral server does not make it permanent.
- **It is not a config default and should not become one.** A server a human
  attaches to must keep the multiplexer contract. See
  [ADR-0063](../ADR/0063-ephemeral-server-lifetime.md).

```sh
# A private, self-terminating server for a scripted run.
phux server --session ci --socket /tmp/phux-ci-$$.sock --exit-after-idle 120 &
```

Lifetime drills:

```sh
# Runtime rule, in process (default test pool).
cargo nextest run -p phux-server server_idle_exit

# Real daemon: exits unattended, reaps its PTY child, and the
# no-flag control stays up.
cargo test -p phux --test idle_exit_e2e -- --ignored
```

## Service-managed pane environment

`phux service install` (ADR-0055) runs the server under launchd or
systemd, both of which start their unit with a minimal environment: no
login shell ever ran, so `PATH` additions a Homebrew or Nix installer
put in `~/.zprofile` / `~/.profile` never take effect. Left alone, every
pane the server spawns would inherit that minimal `PATH` — `nvim` and
`brew` reporting "command not found" even though an ordinary interactive
shell on the same machine has them. [ADR-0073](../ADR/0073-service-managed-pane-login-shell.md)
is the decision record; this is the operator-facing summary.

**The fix is conditional, not a blanket default.** `phux service install`
stamps `PHUX_SERVICE_MANAGED=1` into the unit it generates (both the
launchd `EnvironmentVariables` dict and the systemd `Environment=`
lines). `phux server` checks for that marker at its own startup and,
only when present, spawns every command-less pane's shell in its
platform **login** mode instead of a plain interactive shell:

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
guess from environment shape (a short `PATH`, an unfamiliar parent
process): the same markers that make a profile guard think
initialization already happened (`NIX_PROFILES` and similar) would make
a shape-based guess just as unreliable.

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

phux has two different continuity mechanisms. They are intentionally separate:

- **Restart restore:** `phux workspace save` writes a typed JSON archive of the
  running workspace. `phux workspace restore ARCHIVE` reads that archive and
  creates any missing session names on a running server. Each restored session
  starts a fresh PTY process: the archived `command` is used when present;
  otherwise phux starts the default shell in the archived cwd when available.
  This is a restart/recreate path, not a live handoff path.
- **Live update handoff:** `phux upgrade` is the mechanism intended to keep
  existing PTYs alive across a server binary re-exec. Its e2e drill is
  `cargo test -p phux --test upgrade_e2e -- --ignored`, which checks that a
  pane child PID and scrollback marker survive the upgrade.
- **Release update:** `phux update` is the user-facing verb built on that
  handoff. It resolves the published release, verifies the `.sha256` sidecar
  before unpacking, replaces the binaries atomically, and then calls the
  `phux upgrade` path so panes survive. It writes only to installs it
  maintains — a Homebrew, Cargo, or Nix install gets the exact native command
  instead, and an unrecognized location is refused rather than overwritten
  ([ADR-0074](../ADR/0074-self-update-trust-boundary.md), operator guide in
  [`INSTALL.md`](./INSTALL.md#updating)). The compatibility unit is the
  release: a server, its local clients, its satellites, and its relays must all
  run the same one, because a wire `minor` bump refuses mismatched peers at
  HELLO with no grace window
  ([ADR-0061](../ADR/0061-capabilities-add-versions-break.md),
  [ADR-0071](../ADR/0071-what-phux-1-0-commits-to.md)).

The workspace archive stores sessions, windows, pane metadata, cwd, dimensions,
and split-layout shape where the server reports it. Restore currently recreates
missing **sessions and seed processes** only; it does not replay the archived
split tree into multiple live panes. Do not describe `workspace restore` as PTY
resurrection or full layout replay until a restore-side layout command exists
and has e2e coverage.

Operational smoke checks:

```sh
# Process/cwd restart restore smoke. Starts real phux servers.
cargo test -p phux --test workspace_archive_e2e \
  workspace_restore_starts_archived_command_process -- --ignored

# Save/restore session inventory smoke. Starts real phux servers.
cargo test -p phux --test workspace_archive_e2e \
  workspace_archive_saves_and_restores_sessions -- --ignored

# Live PTY handoff across server update/re-exec.
cargo test -p phux --test upgrade_e2e -- --ignored
```

## Security model and trust boundaries

**Design assumption:** This is not a security-hardened system for hostile environments. It is suitable for trusted networks and multi-user boxes where Unix permissions are enforced by the kernel.

The trust boundary is the operating system user. A phux server trusts every process running as the same UID that can connect to its Unix socket.

### Local trust model (single-machine)

The Unix socket lives in `$XDG_RUNTIME_DIR/phux/` (typically `/run/user/$UID/` on Linux, or `/var/folders/.../T/` on macOS), created with parent directory mode `0o700` (user-only). The OS kernel enforces this boundary at the filesystem level; the socket inherits the parent directory's permissions.

**What this means:**
- Another user on the same machine MAY NOT connect to the socket (kernel-enforced).
- If the parent directory or socket permissions are misconfigured (e.g., accidentally mode `0o777`), the security boundary is breached. **Administrators MUST validate socket permissions in deployment; phux does not re-check at runtime.**
- The process file descriptor table (`/proc/<pid>/fd/<socket-fd>` on Linux) is not readable by other UIDs, so the socket endpoint cannot be enumerated across user boundaries.

### Federation trust model (v0.1+, forward-compatible)

**v0.1 (current):** Remote attach is available for single-server consumers over
WebSocket/TLS and QUIC/TLS. SSH-stdio is built (phux-v45.9): the dialing side
runs `ssh HOST phux stdio-bridge`, delegating authentication and encryption to
SSH; the remote bridge is an ordinary local UDS client on the target host.

**Federation hub (current):** Satellites are phux servers on other machines. A server started with `--hub` dials enabled `[[satellites]]`, aggregates their Terminal inventory, and routes host-qualified Terminal operations over the same wire ([ADR-0007](../ADR/0007-mosh-class-transport-and-satellites.md)). Routes are hub-and-spoke and Terminal-scoped: remote sessions/windows are not merged, and relayed VT bytes remain opaque.

The hub-side enrollment path is two commands: install the hub flag into the
mini's persistent per-user service once, then bootstrap each satellite over
existing SSH trust:

```sh
phux service install --hub
phux host enroll --role satellite user@devbox
```

(Formerly `phux satellite enroll`, a spelling since removed — see
[ADR-0066](../ADR/0066-host-namespace.md).)

`host enroll --role satellite` verifies the remote binary, installs its service, runs
`phux pair --json`, stores the bearer token owner-only, pins the certificate,
and writes the complete local `[[satellites]]` entry. When the satellite has no
dialable listener it falls back to `ssh://user@devbox`; sessions still live on
the satellite, and the hub maintains the SSH-stdio route with capped backoff.
Use `--ssh-only` to choose that route without probing or pairing.

Current remote transports:
- **WebSocket/TCP:** `phux server --listen HOST:PORT`; loopback can be plaintext
  for browser/dev use, while routable binds auto-provision TLS and require a
  `phux pair` bearer token.
- **QUIC/UDP:** `phux server --quic HOST:PORT`; always TLS 1.3 encrypted.
  Routable binds use the same token store and `phux pair` certificate
  fingerprint as the WebSocket path.
- **WebTransport/UDP:** `phux server --webtransport HOST:PORT` (or
  `PHUX_WT_ADDR`); HTTP/3 over QUIC, always TLS 1.3 encrypted — the browser's
  door to QUIC-class transport, dialed by `phux-web` with a WebSocket
  fallback. Routable binds require the same `phux pair` token, carried in the
  CONNECT request: `Authorization: Bearer <hex>` from native consumers, or
  `?token=<hex>` on the session URL from browsers (the JS `WebTransport` API
  cannot set headers); a missing or invalid token is refused with HTTP 403
  before the session exists. Shares the persisted certificate and token store
  with the WebSocket and QUIC paths.
- **SSH-stdio:** `ssh HOST phux stdio-bridge` splices the wire into the
  server's Unix socket on HOST. Reuses established SSH auth (the hub dials
  with `BatchMode=yes`, so key material must work non-interactively);
  inherits SSH's trust model plus the UDS's owner-only local boundary. No
  bearer token or certificate pin on this transport (ADR-0038 addendum).

### Remote consumer trust model (opt-in)

A remote consumer (the native mobile app) can attach over the network without
an SSH tunnel, behind TLS plus a bearer pairing token
([ADR-0031](../ADR/0031-remote-consumer-auth-and-encryption.md)). This is the
nearer-term, single-server path, distinct from the federation hub above.

The bind address is the toggle, so there is no remote-mode setup friction. For
TCP/WebSocket, set it either with `phux server --listen HOST:PORT` or the
`PHUX_WS_ADDR` environment variable (the flag wins when both are present):

- **Loopback address → plaintext, unauthenticated.** The historical
  browser-client dev path; zero config.
- **Routable address → TLS + token, auto-provisioned.** Binding off-loopback is
  treated as exposing the server: phux generates and persists a self-signed
  certificate (under the state dir) if none is configured, and reads the default
  token store. It terminates TLS and requires an `Authorization: Bearer <token>`
  in the WebSocket upgrade; a missing or unrecognized token is refused with HTTP
  401 before any phux frame is read. Plaintext never reaches a routable address.
  Tokens are minted with `phux pair`, which prints the token once alongside the
  certificate's SHA-256 fingerprint to pin out-of-band. Pair before starting a
  network listener: the server loads the token store at startup. Adding or
  deleting a token takes effect after the server restarts.

Native clients can use the same TCP fallback with:

```sh
phux attach --ws wss://HOST:PORT --token HEX --cert-fingerprint FP
```

For UDP/QUIC, set `phux server --quic HOST:PORT` or `PHUX_QUIC_ADDR` and attach
with:

```sh
phux attach --quic HOST:PORT --token HEX --cert-fingerprint FP
```

Use WebSocket/TCP when UDP is blocked by a network or firewall; use QUIC when
roaming/migration behavior matters and UDP is available.

Remote attach has protocol coverage and manual smoke coverage, but it is not a
workspace-restore mechanism. A remote WebSocket or QUIC client attaches to the
server state that exists on that server. It does not move PTYs between hosts,
and it does not replay a saved archive on the remote side by itself. Validate a
remote deployment with a loopback-secure smoke before advertising it:

```sh
# Before starting the server, mint a token and record its fingerprint:
phux pair

# Terminal 1:
PHUX_WS_SECURE=1 phux server --listen 127.0.0.1:8787

# Terminal 2:
phux attach --ws wss://127.0.0.1:8787 --token HEX --cert-fingerprint FP
```

For QUIC, use the same token/fingerprint pair with `phux server --quic
127.0.0.1:8788` and `phux attach --quic 127.0.0.1:8788 ...`.

`PHUX_WS_SECURE=1` forces the secure path on a loopback address (to exercise the
remote path locally); `PHUX_WS_TLS_CERT` + `PHUX_WS_TLS_KEY` substitute an
operator-supplied certificate for the auto-generated one; `PHUX_WS_TOKENS`
overrides the token-store path.

**What this means:**
- The trust boundary widens past the OS user: an authenticated network peer is a
  first-class consumer whose proof is a bearer token over TLS. This is a larger
  attack surface than local UDS. A routable `--listen` address engages TLS and
  token auth automatically; `PHUX_WS_SECURE=1` only forces that path on loopback.
- The token is a bearer credential — anyone holding it is the device until the
  token is revoked. The store is owner-only (`0o600`); the comparison is
  constant-time; tokens are 256-bit from the OS CSPRNG. A client certificate
  (mutual TLS) is the stronger v0.2 hardening recorded in ADR-0031.
- Certificate lifecycle is an operator responsibility, like socket permissions.
  With a self-signed certificate, verifying the `phux pair` fingerprint on the
  device's first connect is what closes the trust-on-first-use MITM window.

### Connecting from another network (overlay reachability)

The remote-consumer path above authenticates and encrypts the link, but it
still needs the client to **reach** the server's address. A self-hosted server
behind NAT/CGNAT/a firewall has no inbound-reachable address, so a phone on
cellular or another Wi-Fi cannot dial it directly — same-network or a VPN is
required.

The sanctioned answer for self-hosters is a **WireGuard-class overlay network**,
which gives the client a routable address that works through NAT
([ADR-0037](../ADR/0037-overlay-network-reachability.md)). phux needs no special
configuration for this: an overlay is an L3 substrate, and phux dials the overlay
address exactly as it dials a LAN address. Because an overlay IP is non-loopback,
the secure path (TLS + token) engages automatically; cert pinning is on the
fingerprint, not the hostname, so MagicDNS-style names work unchanged.

phux is **overlay-agnostic** — pick what fits your trust model:

- **[Tailscale](https://tailscale.com)** — the frictionless on-ramp. Install on
  the server host and the client; attach to the server's `100.x` IP or its
  MagicDNS `*.ts.net` name.
- **[Headscale](https://github.com/juanfont/headscale)** — a self-hostable,
  fully-OSS Tailscale control plane, for operators who will not depend on a
  third-party coordinator.
- **Raw [WireGuard](https://www.wireguard.com), [Nebula](https://github.com/slackhq/nebula),
  or [Netbird](https://netbird.io)** — for hand-rolled overlays. All behave
  identically to phux; it only ever sees an IP.

For step-by-step Tailscale, Headscale, and raw WireGuard walkthroughs, see
[Remote access](./remote-access.md).

```sh
# Before starting the server:
phux pair                            # token + fingerprint, plus the detected overlay address

# Server, reachable on its overlay address:
phux server --listen 0.0.0.0:8787   # binds all interfaces, including the overlay

# Client, from anywhere on the overlay:
phux attach --ws wss://<overlay-host>:8787 --token HEX --cert-fingerprint FP
```

Overlay-address detection in `phux pair` is best-effort (the `tailscale` CLI
when present, else a CGNAT route heuristic); raw-WireGuard operators on
private ranges find the address with their usual tooling. `PHUX_TAILSCALE`
substitutes the CLI that `phux pair` runs (default: `tailscale` on PATH),
mirroring `PHUX_SSH` for the hub dialer.

Hosted relay infrastructure, rendezvous servers, and NAT hole-punching that
would remove the both-ends-install requirement remain out of scope for the
self-host repo (see ADR-0037). The one carve-out, per ADR-0057, is the
self-hosted reference relay below: a relay you run yourself, never one anyone
hosts for you.

### Running the reference relay

phux ships a minimal reference relay in-tree — the runnable artifact behind
the dial-out connector design
([ADR-0051](../ADR/0051-outbound-dial-out-connector-transport.md), ADR-0057).
A server behind NAT dials out to the relay and holds one QUIC tunnel per
named route; remote consumers dial the relay, name the route via TLS SNI, and
are spliced onto that tunnel byte for byte. Be blunt about the trust
tradeoff, because ADR-0051 is: **the relay terminates TLS on both legs, so it
sees every phux frame in plaintext** — every keystroke and every rendered
cell crosses the relay decrypted. The mitigation is not a protocol feature;
it is self-hosting. Run the relay yourself, on a host you trust — that is the
entire reason it ships in this repo. It is a reference relay for self-hosters
and development, not infrastructure software: no accounts, no high
availability, no metrics, no config file. That scope fence is normative
(ADR-0057).

The server-side connector is part of `phux server`. It validates every
`[[connector]]` entry before binding, dials each relay independently, and
redials a lost tunnel with capped exponential backoff. A bad entry fails
startup; a network, certificate, or token failure is logged and retried
without taking the local server down.

The surface is two commands:

```sh
# Enroll a route: mints its tunnel token, provisions the certificate on
# first use, and prints the fingerprint both legs pin.
phux relay pair --route studio

# Run the relay in the foreground (Ctrl-C to stop). --listen has no
# default; binding is always explicit. --max-conns (default 64) is the
# sole limiting knob.
phux relay run --listen 0.0.0.0:4433
```

`phux relay pair` prints the credentials once, in the spirit of `phux
pair`; its output looks like:

```
Tunnel token for route "studio" (a secret — give it to the phux server once):
  <64-hex token>

Relay certificate SHA-256 (pin it on the dialing side to defeat MITM):
  <colon-separated hex fingerprint>

Token written to <state-dir>/relay-tokens
```

Route names ride TLS SNI, so they follow the DNS-label grammar: lowercase
`[a-z0-9-]`, at most 63 characters, no leading or trailing hyphen. Anything
else is rejected with a nonzero exit — never normalized. Pairing an
already-enrolled route replaces that route's token, so rotation is one
command and token and route stay one-to-one. The store rewrite is atomic
(a running relay never observes a torn or empty file), but concurrent
`phux relay pair` invocations are last-write-wins — run one at a time.

`phux relay run` prints the standard `phux` build banner and a listening
line to stderr, then blocks. The listening line carries the resolved bind
address (so `--listen 127.0.0.1:0` shows the OS-assigned port), the
enrolled-route count, and the certificate fingerprint both legs pin:

```
phux <version> (pre-alpha; see docs/spec/)
phux relay listening on 0.0.0.0:4433 (routes=1; cert sha256 <colon-separated hex fingerprint>; Ctrl-C to stop)
```

Lifecycle lines follow on stderr via `tracing`: tunnel up/down per route,
per-reason refusals (bad tunnel token, no live tunnel for an enrolled route,
connection cap reached), and a warning when a newer claim supersedes a live
tunnel (claims are last-writer-wins; that warning is the operator's
theft-detection surface). These lines are a diagnostic surface, not
machine-stable output. Shutdown on Ctrl-C or SIGTERM is immediate — a
reference relay makes no availability promise.

**State files.** Exactly three, at fixed paths in the phux state directory
(`$XDG_STATE_HOME/phux`, or `$HOME/.local/state/phux` when unset) — siblings
of the server's `remote-*` files:

- `relay-tokens` — one `<64-char hex token> <route>` line per enrolled
  route; `#` comments and blank lines are ignored; mode `0600`. The relay
  re-reads this file on every connection attempt, so `phux relay pair` takes
  effect on a running relay and deleting a line revokes at the next
  handshake — no restart, no reload signal. A live tunnel survives its
  token's deletion until it drops or the relay restarts; restarting the
  relay is the immediate revocation path.
- `relay-cert.pem` / `relay-key.pem` — the relay's self-signed TLS pair,
  provisioned on first use and left untouched when both files exist, so the
  pinned fingerprint stays stable across restarts. Operator-supplied
  certificates work by placing PEM files at these paths. The key is written
  mode `0600`.

There are no path flags and no `PHUX_RELAY_*` environment variables. Listing
enrollments is reading the file; revoking is deleting a line; re-pairing
rotates.

**Enrollment flow.**

1. On the relay host, enroll the route: `phux relay pair --route studio`.
2. Copy the printed tunnel token to an owner-only file on the server host,
   then register the relay endpoint and its printed certificate fingerprint:

   ```sh
   install -m 600 /dev/stdin ~/.local/state/phux/relay-studio.token
   # paste the 64-hex tunnel token, then EOF
   ```

   ```toml
   [[connector]]
   relay = "relay.example:4433"
   token-file = "/home/me/.local/state/phux/relay-studio.token"
   cert-fingerprint = "AB:CD:..."
   ```

   The secret stays in the file, never in `config.toml`. Routable relays
   require both fields. Loopback alone may omit them for development.
3. Mint the server's ordinary consumer credential with `phux pair` if the
   remote device does not already have one. The connector authenticates
   consumers against that same server token store; relay enrollment grants
   no access to a terminal.
4. Start or restart `phux server`. With no selector it supervises every
   configured connector. `phux server --connect relay.example:4433` selects
   exactly the matching entry and its credentials; an unconfigured ad-hoc
   endpoint is allowed only on loopback.
5. Consumers dial the relay as if it were the server, naming the route with
   SNI and pinning the relay's certificate:

   ```sh
   phux attach --quic RELAY_HOST:4433 --tls-server-name studio \
     --cert-fingerprint RELAY_FP --token SERVER_TOKEN
   ```

   `RELAY_FP` is the relay fingerprint from `phux relay pair` — the
   consumer's TLS terminates at the relay, so that is the certificate it
   sees. `SERVER_TOKEN` is the server's own `phux pair` token: it crosses
   the relay as opaque bytes and is verified by the server, never by the
   relay.

The connector token file is re-read on every dial attempt. To rotate it,
run `phux relay pair --route studio` again, atomically replace the server's
owner-only token file, then force a redial (restart the server or relay) if
the old live tunnel must be revoked immediately. Otherwise the next natural
redial picks up the new token. Deleting the route from `relay-tokens` refuses
future dials but, like rotation, does not sever the current tunnel by itself.

Refusals are distinguishable at the consumer. An unknown or absent route
name fails during the TLS handshake itself — no phux bytes are exchanged,
and the relay is indistinguishable from a non-phux TLS endpoint. An enrolled
route with no live tunnel completes the handshake and then closes with a
distinct route-offline application code. "Wrong route name" and "server not
dialed in" therefore produce different failures.

### Output mode for remote consumers

A remote phone link is high-latency and may be lossy. A remote consumer SHOULD
request `OutputMode::StateSync` ([ADR-0018](../ADR/0018-lazy-state-synchronization.md))
at HELLO rather than the default `OutputMode::Raw`: StateSync ships the minimum
VT to move the consumer's last-acked state to canonical per tick, coalescing
floods and pacing per-consumer RTT. Raw stays the default for local interactive
peers, where byte-faithful pass-through is lowest-latency on a fast link.

### Known limitations

- **Local transports are plaintext:** UDS and explicit loopback WebSocket carry
  plaintext. UDS relies on filesystem permissions; loopback WS is a development
  path. Routable WSS and QUIC listeners use TLS.
- **Scrollback unencrypted:** Terminal history is stored in the libghostty grid in RAM, unencrypted. A memory dump can recover it.
- **Encryption belongs to the transport:** phux frames have no independent
  per-command encryption. WSS and QUIC protect the complete stream with TLS.
- **No audit logging:** phux does not log which user accessed which terminal or when. Can be added as future hooks.
- **SSH is the trust boundary for remote attach (v0.1):** phux does not perform additional authentication over SSH; it delegates entirely to SSH key management and host verification.

### What you DO get

- **Kernel-enforced permission boundary:** On Linux and macOS, the OS prevents other users from connecting to your socket.
- **No privilege escalation surface:** The server runs as your user (not setuid/setgid). A compromised terminal cannot elevate to other UIDs.
- **No eval RPC:** phux does not evaluate source text inside the server, but an
  authenticated consumer can spawn commands and drive shells with the server
  user's authority. Treat a remote token as command-execution access.
- **Process isolation via OS:** Each terminal's PTY is managed by the kernel; one terminal's PTY cannot directly access another terminal's memory or file descriptors.
