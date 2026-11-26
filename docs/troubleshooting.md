---
group: help
order: 0
---

# Troubleshooting

Known failures and their fixes. Start with `phux doctor`: it checks config,
socket, server, certificates, and listeners, and prints a fix beside each
problem. It only reads, and exits 1 only when a check fails.

```sh
phux doctor                 # this machine
phux doctor --remote mini   # another host, with each fix as a command to run there
phux status                 # pid, uptime, protocol, clients
phux config check           # is the config valid
phux logs --server -f       # follow the server log; --client for the terminal UI
```

[File locations](./reference/files.md) lists every log and state path.

### What works here

After its checks, doctor prints a checklist (agent status, notifications,
remote reach, file transfer, restore, the agent skill), each with a reason
and a fix. A capability that is off is marked `–`, not as a failure, and
the checklist never changes doctor's exit code. Clients read the same list; its JSON is in
[Host checklist](./building.md#host-checklist).

## Reporting a bug

<kbd>C-a</kbd> <kbd>B</kbd> in the terminal UI writes a report with
versions, logs, and the screen to the state directory. `phux report new`
writes one without the screen; `phux report show` prints the newest.
`phux version` prints client and server versions. In Phux Rune, use
**Copy diagnostics** in Settings.

**A report can contain secrets** from your panes. Read it and remove
anything private before you share it. Then file
[an issue](https://github.com/no-phux/phux/issues) with your OS, versions,
install method, and steps.

## The server

**`no server running at ...`.** `phux` or `phux new` starts one; read-only
commands such as `ls` and `snapshot` do not.

**The server didn't start.** The error names the server log. Usually the
config does not parse (`phux config check`), or a network listener is set
while `[policy] mode` is unset; see [Who can connect](./remote.md#who-can-connect).

**The server keeps restarting.** `phux doctor` reports it as crash-looping
and names the log. If it warns about an old service unit,
`phux service reconcile` fixes the unit without stopping anything.

**My sessions are gone.** Check you reach the right server: a binary run
from a checkout uses the `dev` profile, and `PHUX_PROFILE` picks another.
`phux doctor` warns on its `instance` line. Otherwise see
[After a crash or reboot](#after-a-crash-or-reboot).

## Versions

**`incompatible protocol: client offered X, server requires Y`.** An older
server is still running after an update. `phux doctor` names its socket and
PID (if it names none, `lsof -nP /path/to/phux.sock` shows the owner).
Check it with `ps -p PID -o pid=,args=`, stop that PID with `kill PID`,
then run `phux server --ensure`. Never kill by name or port; other profiles
may be running. Stopping the server ends its panes' programs.

**`the running server is X; this binary is Y`.** Run `phux upgrade`. Panes
survive.

**Update fails with `new binary failed validation` and an `extends layer
... No such file or directory`.** The upgrade checks your config first. Fix
the missing path in `extends`, run `phux config check`, and retry.

## Attaching

**`interactive use requires both stdin and stdout to be terminals`.** Use
the headless commands in [Scripting](./scripting.md).

**`refusing to attach from inside one of this server's own panes`.** Switch
with <kbd>C-a</kbd> <kbd>s</kbd>, or set `PHUX_ALLOW_NESTED=1` to nest.

**Remote attach fails.** See [Remote machines](./remote.md#troubleshooting).

## Networks

**A VPN other than Tailscale.** Tell phux this machine's address on it, in
the shell and where the server starts:

```sh
PHUX_OVERLAY_ADDRS=100.92.1.7 phux pair
```

For a `phux service install` server, run `systemctl --user edit phux.service`,
add `Environment=PHUX_OVERLAY_ADDRS=100.92.1.7` under `[Service]`, then
`phux upgrade`.

**A paired device keeps reconnecting.** If `phux doctor` warns
`remote-replies`, this machine has no route back to the device. Check with
`route -n get ADDR` (macOS) or `ip route get ADDR` (Linux) and fix the
route.

**A phone will not pair.** It must share the network in the link, with one
VPN or DNS profile active. On iOS, allow Local Network for the app in
Settings. Otherwise pair through a
[relay](./remote.md#relays).

## Keys, clipboard, and agents

**Keys do nothing.** `phux config check` reports bindings that don't parse;
`phux config keys` prints the bindings in effect. See
[Key bindings](./keys.md).

**Copy doesn't reach the clipboard.** Copy uses OSC 52; allow it in your
terminal.

**A program on a headless host can't open a browser.** Set
`[defaults] browser-shim = true` there and restart its server. The terminal
UI or Rune asks whether to open the link on your machine; declined, or with
neither typing in that pane, the link is printed there and copied to your
clipboard ([Opening links](./ssh.md#opening-links)).

**An agent's state is wrong.** See [Debugging](#debugging). Override screen
rules in `~/.config/phux/agent-rules/`.

## Debugging

```sh
phux agent explain @7                                # which evidence decided the state
phux agent explain --file screen.txt --kind claude   # test rules on a saved screen
phux watch --json @7                                 # events as they arrive
phux agent log @7                                    # an agent session's events
```

## Lag

```sh
phux perf --watch 1          # server timings every second
phux perf trace --seconds 5  # a trace to open in https://ui.perfetto.dev
```

See whether time goes to input, program output, or a slow client before
blaming the network. MCP clients get the same from `phux_perf` and
`phux_perf_trace`.

## After a crash or reboot

A crash, reboot, or server stop ends the programs in your panes; the
sessions come back empty. `phux ls` lists them first, and `phux restore`
relaunches them ([what survives what](./concepts.md#what-survives-what)).

A pane shown as **lost** may still have a running process that phux no
longer controls. No command recovers it. Do not kill a guessed PID or edit
the store; save `phux ls --json`, the error, and the logs, and file a
report.

## Downgrading

An older phux refuses a newer store with `org store schema version N is
newer than this binary reads (M)`. Before installing an older release:

1. `phux archive export -o sessions.json`
2. `phux kill --server`
3. Back up the state directory's `org/` and `work/` folders.

Never move `org/` or `work/` aside to start empty: the server loses track of
every pane. The `work/` store migrates forward on first open and an older
binary then refuses it.
