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

**My panes are gone after a reboot.** The processes are. The sessions,
tabs, and pane records are not; `phux ls` lists them with their panes
marked as ended. `phux restore` starts them again in the same directories.
`phux service install` keeps the server running across logout and reboot,
so this only happens on a cold boot.

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

## Where things are

[Files](./reference/files.md) lists every path: the socket, config, state,
logs, credentials. A development build (one not installed from a release)
uses `phux-dev` directories and its own socket, so it never shares state
with the installed one.

## Exit codes

0 ok, 1 failure, 2 usage error or refused by the server, 3 partial fleet
view, 124 `wait`/`watch` timeout, 125 `run` timeout.
[Exit codes](./reference/exit-codes.md) has the table.
