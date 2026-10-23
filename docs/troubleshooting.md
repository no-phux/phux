# Troubleshooting

When something in phux doesn't work, start with `phux doctor`. It checks
your config, the socket, the server, certificates, and listeners, and prints
a fix next to each problem. This page lists the failures people hit, with
the exact message and what to do.

```text
$ phux doctor
phux: ok:    config             /Users/me/.config/phux/config.toml is valid
phux: ok:    instance           profile default; state /Users/me/.local/state/phux
phux: ok:    socket-path        /tmp/phux-me/phux.sock
phux: ok:    server             reachable at /tmp/phux-me/phux.sock (2 session(s), 5 pane(s)); client protocol 1.0.0
phux: ok:    server-health      0 server start(s) in the last hour
phux: ok:    plugins            3 manifest(s) valid
phux: ok:    agent-shim         no claude-in-phux shim installed
phux: warn:  remote-cert        no auto-provisioned certificate at /Users/me/.local/state/phux/remote-cert.pem
  hint: remote-cert: run `phux pair` — it provisions the certificate and mints a device token
...

phux: warn: no failures, 1 warning(s)
```

`phux doctor` only reads. It exits 1 when a check fails; warnings alone exit
0, because a stopped server is normal. `--json` prints the same checks as a
document.

## Inspecting state

```sh
phux doctor            # config, socket, server, certificates, listeners
phux status            # pid, uptime, protocol, clients, sessions
phux config check      # whether the config file is valid
phux logs              # where the logs are
phux logs --server -f  # follow the server log
phux logs --client     # the newest terminal UI log
```

The terminal UI writes its log to a file, never to the screen. The server writes
`server.log` in the state directory however it was started.
[File locations](./reference/files.md) lists every path: the socket, config,
state, logs, and credentials.

## Reporting a bug

Press <kbd>C-a</kbd> <kbd>B</kbd> in the terminal UI. It writes a report with
versions, logs, and the current screen to the state directory. From a shell,
`phux report new` writes one without the screen, and `phux report show`
prints the newest.

**A report can contain secrets.** The screen and logs may include whatever
was in your panes. Read it, and remove anything private, before you share
it. phux never logs what you type or paste, and its log files are readable
only by you.

## The server

**`no server running at /tmp/phux-me/phux.sock`.** Nothing is listening on
the socket. `phux` or `phux new` starts a server. Read-only commands such as
`ls`, `status`, and `snapshot` report this instead of starting one.

**The server didn't start.** The error names the server log. The usual
causes are a config file that doesn't parse (run `phux config check`), or a
network listener configured while `[policy] mode` is unset. Set it to
`"paired"`, or remove the listener settings. See
[Remote machines](./remote.md#who-can-connect).

**Stale socket.** A server that died without cleaning up leaves its socket
file behind. The next command notices nothing answers, removes it, and
starts a fresh server. `phux kill --server` says
`no server running at ... (reaped a stale socket)` in that case.

**The server keeps restarting.** `phux doctor` reports
`the server started N times in the last M minutes — it is crash-looping`,
with the log that has the reason. It also warns when an older service unit
restarts the server on every exit; `phux service reconcile` fixes the unit
in place without stopping anything.

**My sessions are gone.** First check that you are talking to the right
server. A build that isn't an installed release uses its own `phux-dev`
directories and socket, and `PHUX_PROFILE` picks another instance; either
way `phux doctor` warns on its `instance` line. If that is fine, see
[After a crash or reboot](#after-a-crash-or-reboot).

## Versions

**`incompatible protocol: client offered X, server requires Y`.** The binary
and the running server speak different protocol majors, usually because you
updated and the old server is still running. Releases on the same major
work together in either direction. Attaching with `phux` upgrades an older
server automatically; a newer one is left running. From a script,
`phux upgrade` moves the server onto the installed binary with every pane
kept.

**`the running server is X; this binary is Y`.** The same fix: `phux upgrade`.

## Attaching

**`interactive use requires both stdin and stdout to be terminals`.** You ran
`phux` or `phux attach` from something that isn't a terminal. Use the
headless commands from [Shell scripting](./scripting.md) there.

**`refusing to attach from inside one of this server's own panes`.**
Attaching from a pane of the same server would draw the terminal UI inside itself.
Switch sessions with <kbd>C-a</kbd> <kbd>s</kbd>, or set
`PHUX_ALLOW_NESTED=1` if you mean to nest.

**Remote attach fails.** See
[Remote machines: troubleshooting](./remote.md#troubleshooting).

## Keys, clipboard, and agents

**Keys do nothing, or the prefix is wrong.** `phux config check` reports
bindings that don't parse; each one is skipped on its own. A prefix that
doesn't parse falls back to <kbd>C-a</kbd>. `phux config keys` prints every
binding in effect and where it came from, and <kbd>C-a</kbd>
<kbd>Space</kbd> lists every action with its key. See
[Key bindings](./keys.md).

**Copy doesn't reach my clipboard.** Copying uses OSC 52, which your
terminal must allow. Most do, some behind a setting. Over SSH it works if
your local terminal allows it.

**An agent's state is wrong.** `phux agent explain @N` shows the evidence
behind the state: which source won and what it saw. If the screen rules for
an agent misfire, override them in `~/.config/phux/agent-rules/`. See
[Coding agents](./agents.md#agent-states).

## Lag

`phux perf --watch 1` prints the server's timings every second, so a stall
shows up in the second it happened. Without `--watch`, the numbers cover the
server's whole lifetime. Check whether time goes to waiting on input, to
processing program output, or to a slow client before you blame the network
or restart anything.

To see where the time goes, record a trace and open the file it prints in
<https://ui.perfetto.dev>:

```sh
phux perf trace --seconds 5     # the server
phux perf trace --desktop       # the desktop app and the server, on one timeline
```

Start either process with `PHUX_DEV=1` to add memory figures, or turn on
**Settings > Developer > Developer mode** in the desktop app. A desktop app in
Developer mode also shows a frame-rate overlay, and `phux perf --desktop`
reads it. Agents get the same numbers from the `phux_perf` and
`phux_perf_trace` MCP tools.

## After a crash or reboot

`phux upgrade` and `phux update` keep every program running. A crash, a
reboot, or stopping the server does not: the programs in your panes end with
it. A service started by `phux service install` brings a server back after a
reboot, but it can't bring back processes.

phux keeps the record of your sessions and panes, so you can see what ended
and start it again:

```sh
phux ls            # sessions that need you come first, including work a restart lost
phux restore       # relaunch every pane a server restart ended
```

phux never relaunches or adopts processes on its own. After a crash, a pane
can show as lost: its process may still be running, but phux no longer
controls it. **There is no supported command yet to recover a lost pane.**
Don't kill a guessed PID, edit the store, or restart other work to get past
the refusal. Save the output of `phux ls --json`, the error, and the logs,
and file a report.

## Downgrading

An older phux refuses a store written by a newer one, with
`org store schema version N is newer than this binary reads (M)`. Before you
install an older release:

1. Export your sessions with `phux archive export -o sessions.json`.
2. Stop the server with `phux kill --server`.
3. Back up the state directory's `org/` and `work/` folders while the server
   is stopped.

**Never move the `org/` or `work/` folder aside to start empty.** The
server would lose track of every pane it was running. A newer server keeps a backup before it
migrates an existing store.

Some upgrades can't be undone by installing the old binary:

- The `work/` store, which holds agent mail and claims among other
  things, moves to a new format the first time a newer binary opens it,
  and an older binary then refuses it.
- The `org/` store can hold a recent operation an older server doesn't
  know, such as a project link or a session kill. The older server refuses
  the store until a newer one has pruned that record, after a day or
  10,000 newer records.
- An older server ignores a session's project link.

## Exit codes

Every command exits 0 on success, 1 on failure, and 2 on a usage error or
a refused request. [Shell scripting](./scripting.md#exit-codes) lists the
rest, and [Exit codes](./reference/exit-codes.md) has the full reference.
