# Troubleshooting

What to run first, and the failures people hit, with what each one means.

## Start here

```sh
phux doctor            # config, socket, server, certificates, listeners
phux status            # pid, uptime, protocol, clients, sessions
phux logs              # where the logs are
phux logs --server -f  # follow the server log
phux config check      # is the config file valid
```

`phux doctor` is read-only and prints a remedy next to each failed check.
To file a bug, press `Ctrl-A B` inside the TUI, which bundles versions,
logs, and the current screen into a report under the state directory, or
run `phux report new` from a shell. `phux report show` prints the newest.
Review/redact reports before sharing: screen contents can contain secrets.
Runtime input logging must remain structural, never typed text, codepoint
content, paste/file bytes or private keys; logs are owner-only (0600). See
[redaction tests](../crates/phux-protocol/src/input/mod.rs) and
[log permissions](../crates/phux-server/src/telemetry.rs).

## Things that go wrong

**"no server running at ..."** Nothing is listening on the socket. `phux`
or `phux new` starts one. Read-only commands (`ls`, `status`, `snapshot`)
report this instead of starting a server.

**The server did not start.** The error names the server log. The usual
causes: the config file does not parse (`phux config check`), or a listener
is configured while `[policy] mode` is unset; set it to `"paired"` or
remove the listener settings (see [remote](./remote.md)).

**Stale socket.** A server that died without cleaning up leaves its socket
file behind. The next command notices nobody answers, removes it, and
starts a fresh server. `phux kill --server` reports "reaped a stale
socket" in that case.

**"incompatible protocol: client offered X, server requires Y".** The
binary and the running server speak different protocol versions, usually
because you updated and the old server is still running. `phux` upgrades
the server automatically on attach; from a script, `phux upgrade` restarts
the server onto the installed binary with its panes intact.

**"interactive use requires both stdin and stdout to be terminals".** You
ran `phux` or `phux attach` from something that is not a terminal. Use the
headless commands ([scripting](./scripting.md)) there.

**Refused to attach from inside a pane.** Attaching to the server from one
of its own panes is refused, since it would nest. `Ctrl-A s` switches
sessions from inside the TUI; `PHUX_ALLOW_NESTED=1` overrides the check.

**My panes are gone after a reboot.** Durable records are not preserved
PTYs. Inspect `phux ls` and explicitly restore eligible ended work; a Lost
activation needs reconciliation, not blind relaunch. A login service
starts a new server after reboot; it cannot keep a process alive through
reboot. See [cold restart](#cold-restart-and-downgrade).

**The server keeps restarting.** `phux doctor` reports a crash loop from
`server-starts.log`, and whether the service unit's restart policy is the
current one (`phux service reconcile` fixes an old unit).

**Keys do nothing / the prefix is wrong.** `phux config check` reports
bindings that did not parse; they are skipped individually. A prefix that
does not parse falls back to `Ctrl-A`. `Ctrl-A Space` lists every action
with its current key.

**Copy does not reach my clipboard.** Copying uses OSC 52, which the
outer terminal must allow; most do, some behind a setting. Over SSH it
works if the local terminal allows it.

**Remote attach fails.** See [remote: when it does not connect](./remote.md#when-it-does-not-connect).

**An agent's state is wrong.** `phux agent explain @N` shows the evidence
behind the decision: which source won and what it saw. If the screen rules
for an agent misfire, override them in `~/.config/phux/agent-rules/`.

## Lag and crash evidence

Use [`phux perf --watch 1`](./reference/cli.md#phux-perf) to compare intervals;
a one-shot sample covers the server lifetime. Separate input credit waits/
timeouts from PTY parse/fan-out time and slow client output before blaming
the network or restarting work. The [metric inventory](../crates/phux-server/src/perf.rs)
and [bounded stream diagnostics](../crates/phux-server/src/stream_diagnostics.rs)
are the current names, not a second hand-maintained metric table.

To see where the time goes, record a trace: `phux perf trace --seconds 5`
for the server, or `phux perf trace --desktop` for the desktop and server
on one timeline. Open the file it prints in <https://ui.perfetto.dev>.
Start either process with `PHUX_DEV=1` to add heap figures. A desktop in
dev mode also shows a HUD with frame rate, frame build time and jank, and
answers `phux perf --desktop`. Agents read the same numbers through the
`phux_perf` and `phux_perf_trace` MCP tools.

TUI logs go to a file, never alt-screen stderr. Server stderr is captured
by its spawner/service (with an optional file tee). Keep the failing process
and profile straight; a daemon failure can affect many panes. For an
authorized isolated reproduction, `RUST_BACKTRACE=1` records panic context.
[Telemetry](../crates/phux-server/src/telemetry.rs) owns log routing and panic
hooks. Preserve evidence and redact reports before sharing.

## Cold restart and downgrade

Hot `phux upgrade` hands live PTYs to the replacement binary; a cold start,
crash or reboot does not. [Organization reconciliation](./organization.md#activations-and-reconciliation)
checks boot/PID/start identity and never automatically relaunches or adopts
arbitrary processes. Lost means a recorded process may still run without its
engine. **There is currently no supported end-to-end CLI procedure to
terminate/reconcile a post-crash Lost record and then relaunch it.**

1. Read and preserve the inventory with `phux ls --json`. In that document,
   `sessions[].exec.lost` counts Lost members and `sessions[].key` identifies
   the session. Save the refusal, logs, and any known resource key; do not
   reuse an old `@N` or treat a PID as the activation identity.
2. Leave the uncertain record intact and escalate to the serving-host
   operator/maintainer. CLI inventory does not expose the complete persisted
   process identity for each Lost activation. `resource show` inspects live
   resources, not that missing engine. There is no CLI `resource terminate`
   or `reconcile` verb; ordinary `kill`/`signal` is not a documented substitute.
   Do not guess a PID kill, edit/delete the store, or cold-restart other live
   work to bypass the refusal.
3. A maintainer must establish identity and an inactive activation before
   explicit `phux relaunch ^RESOURCE_KEY` (substitute the verified key).
   Startup reconciliation can mark a no-longer-matching process
   Completed/Unobserved at a later planned start, but it is not an on-demand
   CLI operation or a reason to restart production for diagnosis. If the
   state cannot be verified, stop; no safe relaunch is established.

For maintainers, `GET_ORGANIZATION`/`phux_client::org::fetch_from_socket`
exposes the record key, activation id/instance/state and optional live
handle. Post-cold-start Lost records have no engine-backed live handle; the
public termination helpers require one. Core Lost-termination semantics
therefore do not establish a usable CLI recovery path. See the
[record projection and termination target](../crates/phux-server/src/runtime/organization.rs),
[client helpers](../crates/phux-client/src/org.rs) and
[crash-reconciliation test](../crates/phux-server/tests/lifecycle/crash_restart.rs).

Before an incompatible downgrade, export the supported organization/archive
view, deliberately stop the owning server, and preserve its store plus
backups using a consistent SQLite backup (or the complete stopped database,
including any WAL state). A newer store schema is refused; migration makes
a backup before changing an existing schema. Never rename away the store
and start empty as a silent fallback: that loses execution ownership and
receipts. See [store behavior](../crates/phux-server/src/org_store.rs) and
[restore options](./reference/cli.md#phux-restore).

A refused remote policy during hot handoff must keep adopted local work
served, disable/report the remote door, and avoid killing adopted PTYs.
Cold start with the same invalid posture refuses. The
[resume-posture test](../crates/phux-server/src/runtime/mod.rs) owns this
fail-safe distinction.

## Where things are

[Files](./reference/files.md) lists every path: the socket, config, state,
logs, credentials. A development build (one not installed from a release)
uses `phux-dev` directories and its own socket, so it never shares state
with the installed one.

## Exit codes

0 ok, 1 failure, 2 usage error or refused by the server, 3 partial fleet
view, 124 `wait`/`watch` timeout, 125 `run` timeout.
[Exit codes](./reference/exit-codes.md) has the table.
