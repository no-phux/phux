---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-08-01
---

# Process model

**TL;DR.** One server per user hosts every session for that user; clients
are separate processes attached over a Unix socket. The single `phux`
binary contains both halves and dispatches by subcommand; an attach
auto-spawns a server if none is listening. Runtime paths live under
`$XDG_RUNTIME_DIR/phux/`; persistent per-user state — logs, TLS
material, token store — lives under `$XDG_STATE_HOME/phux/`.

---

The runtime path resolution lives in
[`phux-server/src/runtime/mod.rs`](../../crates/phux-server/src/runtime/mod.rs): the
socket is `$XDG_RUNTIME_DIR/phux/phux.sock` when that variable is set,
otherwise `/tmp/phux-$UID/phux.sock`. The parent directory is created
mode `0o700`.

The persistent per-user state directory is real. It resolves via
`phux_server::telemetry::state_dir()` in
[`phux-server/src/telemetry.rs`](../../crates/phux-server/src/telemetry.rs)
to `$XDG_STATE_HOME/phux/` (falling back to `$HOME/.local/state/phux/`
when `XDG_STATE_HOME` is unset). Its current inventory:

```
$XDG_RUNTIME_DIR/phux/phux.sock     # SOCK_STREAM, perms 0o700 dir
$XDG_STATE_HOME/phux/               # per-user state dir (telemetry::state_dir)
├── server.log                      # THE canonical server log, both spawn paths
│                                   # (telemetry::server_log_path)
├── client-<pid>.log                # per-pid client/TUI logs
├── remote-tokens                   # versioned remote credential store (ADR-0031)
├── remote-cert.pem                 # auto-provisioned TLS cert (ADR-0031)
├── remote-key.pem                  # auto-provisioned TLS key (ADR-0031)
├── service-wrapper.sh              # `phux service install --restore` wrapper
└── workspace.json                  # `--restore` workspace snapshot
```

Server logging: a hand-started foreground `phux server` logs to stderr
(plus an optional `PHUX_LOG` file tee). Both detached spawn paths — the
auto-spawn daemon and a `phux service`-installed unit — redirect the
server's stderr to the one canonical `server.log` above, resolved
through `telemetry::server_log_path()` so writers and readers
(`phux service logs`) can never disagree. The startup line carries
pid + version + socket to attribute interleaved writers.

Still **design intent, not yet implemented**: a `server.pid` file and a
`journal/` directory of per-pane PTY output for crash recovery. Today
the server keeps session state only in memory.

The single `phux` binary contains both server and client logic; the
subcommand dispatches. `phux server` runs the daemon in the foreground;
`phux` (no args) becomes a client and lazily spawns a server if none is
listening on the socket. The auto-spawn follows tmux's convention so a
user never has to start a daemon by hand.

Inside the server, a PTY-backed terminal actor now runs **two** independent
timers on its `select!`: the state-sync tick that paces output emission to its
consumers, and a second, slower agent-state detector tick
([ADR-0046](../../ADR/0046-server-side-agent-state-detection.md)) that
re-derives the pane's `phux.agent/v1` record from the PTY's foreground process,
the OSC title, and the live screen, and publishes the privacy-bounded
`phux.pane-occupant/v1` foreground basename/shell answer from the same process
query. The detector timer is the sole driver of
that work — PTY bytes never wake it — so a chatty pane costs no extra
detection. It is constructed only for a PTY-backed actor, only when a rule set
loaded, and it publishes through its own `mpsc` channel to a per-terminal drain
task that owns the metadata write. No new process, no new thread.
