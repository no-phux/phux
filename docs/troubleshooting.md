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

### What works here

Doctor ends with a checklist of what you can expect from this host, each
with the reason and the fix:

```text
What works here
phux: error: Agent status          no agent integration for OpenCode
  fix: opencode plugin add @phux/opencode
phux: ok:    Asks and notifications 1 phone gets notifications from this host
phux: warn:  Remote reach          quic listener waiting to bind: address not available yet
phux: ok:    File transfer         files can be sent to this host and fetched from it
phux: error: Restore after restart server not supervised, sessions end at logout
  fix: phux service install --adopt
phux: ok:    Agent skill           phux skill installed for claude, codex
```

phux mobile shows the same list for each host: it asks the server
(`HOST_CHECKLIST`), and the server answers with the `checklist` of its own
`phux doctor --json`. The checklist never changes doctor's exit code.

In `--json` it is `checklist`, a versioned document:

```json
{
  "schema_version": 1,
  "items": [
    {
      "id": "notifications",
      "title": "Asks and notifications",
      "status": "off",
      "reason": "no phone has turned on notifications for this host",
      "fix_command": null,
      "fix_action": "enable_notifications",
      "push_devices": []
    }
  ]
}
```

| Field | Meaning |
| --- | --- |
| `id` | `agent_status`, `notifications`, `remote_reach`, `file_transfer`, `restore`, `skill`, in that order. Later versions may add ids; skip ones you don't know. |
| `status` | `ok`, `off` (does not work), `waiting` (will work without help: a listener retrying its bind, or supervision armed for the next start), or `unknown` (could not be checked, such as with no server running). |
| `reason` | Why, in plain words. |
| `fix_command` | What to run on the host, one command per line, or `null`. |
| `fix_action` | What a client can do itself in one tap: `enable_notifications`, or `null`. |
| `push_devices` | `notifications` only: the device ids holding a push grant (`phux.push/v1/<device>`), so a phone can tell whether it is one of them. |

Adding an item or a field keeps `schema_version`; renaming or removing one
bumps it.

What each item checks:

- **Agent status**: every coding agent found on the host has the phux
  integration that reports its state ([Agent setup](./agents.md#agent-setup)),
  and the `phux agent install-claude` wrapper is current. An agent with no
  integration yet is read from the screen.
- **Asks and notifications**: at least one phone holds a push grant.
- **Remote reach**: the server's remote listeners are bound, and its routable
  WebSocket listener answers.
- **File transfer**: the server serves `phux upload` and `phux download`.
- **Restore after restart**: launchd or systemd supervises the server; on
  Linux, lingering is on so the user's units outlive the login session.
- **Agent skill**: `phux skill setup` has taught your agents about phux.

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

In Phux Rune, **Copy diagnostics** in Settings copies its logs and state; [log locations](../clients/rune/README.md#what-it-does).
File [an issue](https://github.com/no-phux/phux/issues) with your OS/CPU,
CLI and desktop versions, install method, reproduction steps, and actual error.

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

**Desktop update says `new binary failed validation` and names an `extends
layer ... No such file or directory`.** The server upgrade checks your
configuration before replacing the running server image. The CLI may already
have been updated. Run `phux config check`, then correct the
missing path in your config's `extends` array. A distribution layer pointing
inside a source checkout must follow that checkout if you move it. Keep the
layer's plugin files with it; removing the import also removes its settings.
Run `phux config check` again before retrying the update. This error is a
missing file, not a macOS permission refusal.

**`incompatible protocol: client offered X, server requires Y`.** The binary
and the running server speak different protocols, usually because you updated
and an older server is still running. Run `phux doctor` for the affected
profile or explicit socket. Its recovery message identifies the socket and,
when available, the server PID. Inspect that PID with `ps -p PID -o pid=,args=`
before stopping it with `kill PID`, then run `phux server --ensure` against
the same profile or socket. If doctor cannot identify it, inspect the socket
owner with `lsof -nP /path/to/phux.sock`; do not guess from a process name.
Stopping that server closes its panes. Never kill by name or pattern: other
profiles and private servers may be running too.

**Releases on the same protocol major.** They work together in either
direction. Attaching with `phux` upgrades an older server in place; a newer
one is left running. From a script, `phux upgrade` moves the server onto the
installed binary and panes survive.

**`the running server is X; this binary is Y`.** On the same protocol, the
same fix: `phux upgrade`.

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

## Private networks and VPNs

**phux needs no VPN.** A machine you can SSH to, a
[pairing link](./remote.md#pairing-with-a-link) on the same network, and a
[relay](./remote.md#relays) all work without one. A private network is one
more way for two machines to reach each other.

**Tailscale and Headscale.** phux asks the `tailscale` command for this
machine's address. Headscale uses the same client, so it needs nothing
extra. `PHUX_TAILSCALE` names the command when it is not on `PATH`.

**Netbird, WireGuard, ZeroTier, and others.** Tell phux the address this
machine has on that network:

```sh
PHUX_OVERLAY_ADDRS=100.92.1.7 phux pair
```

Set the same variable where the server starts, so its listener binds that
address. It takes several addresses separated by commas; the first is the
one the server binds. phux does not install or configure the network.

**A phone that will not pair.** A phone runs one VPN at a time, and some
DNS profiles (NextDNS, for example) count as one. If the phone is on a
different private network from the link, or a DNS profile is filtering the
name in it, pairing times out. Check in this order:

1. The phone and the machine are on the same Wi-Fi, or the same private
   network, when you scan.
2. Only one VPN or DNS profile is active on the phone.
3. On iOS, the app has Local Network permission in Settings.
4. `phux doctor` on the machine reports the address in the link as
   reachable.

If the two cannot share a network, pair through a
[relay](./remote.md#relays).

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

**A program on another machine tries to open a browser and nothing
happens.** A sign-in started in a pane on a server with no screen (`gh auth
login`, a cloud CLI) has no browser to open. phux can give its panes a
stand-in that hands the link to you instead: set `browser-shim = true` under
`[defaults]` in that machine's config and restart its server. To do the same
by hand, or for a shell outside phux, save this as `~/.local/bin/phux-browser`
there and make it executable:

```sh
#!/bin/sh
printf '\033]52;c;%s\a' "$(printf %s "$1" | base64 | tr -d '\n')" > /dev/tty
printf 'Open this on your own machine (copied to your clipboard):\n%s\n' "$1" > /dev/tty
```

Then set `BROWSER="$HOME/.local/bin/phux-browser"` in that machine's shell
startup file. A program that opens a link now prints it in the pane and
puts it on your clipboard, through the same OSC 52 path as copy. Nothing
opens by itself. <kbd>C-a</kbd> <kbd>U</kbd> lists the pane's links if you
need it again.

**An agent's state is wrong.** `phux agent explain @N` shows the evidence
behind the state: which source won and what it saw. If the screen rules for
an agent misfire, override them in `~/.config/phux/agent-rules/`. See
[Coding agents](./agents.md#agent-states).

## Debugging

- `phux agent explain @7` shows which evidence decided the state, and what
  each rule saw.
- `phux agent explain --file screen.txt --kind claude` tests rules against a
  saved screen without a server.
- `phux watch --json @7` shows the events, such as `agent_state` and
  `asked`, as they arrive.
- `phux agent log @7` prints an agent session's events.
- `phux doctor` checks the Claude Code wrapper.

## Lag

`phux perf --watch 1` prints the server's timings every second, so a stall
shows up in the second it happened. Without `--watch`, the numbers cover the
server's whole lifetime. Check whether time goes to waiting on input, to
processing program output, or to a slow client before you blame the network
or restart anything.

To see where the time goes, record a trace and open the file it prints in
<https://ui.perfetto.dev>:

```sh
phux perf trace --seconds 5
```

Start the server with `PHUX_DEV=1` to add memory figures. Agents get the same numbers from the `phux_perf` and
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

Unless you set `[restore] on-start = true`
([what survives what](./concepts.md#what-survives-what)), phux never
relaunches processes on its own, and it never adopts them. After a crash, a pane
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

## Plan usage

`phux agent usage` prints the AI plan limits this machine can see: Claude
Code's 5-hour and weekly windows, Codex's limits, and Grok Build's
allowance, with how much is used and when each resets.

It reads each vendor's own sign-in on this machine and sends it only to that
vendor:

| Vendor | Reads | Sends, and only to |
| --- | --- | --- |
| Claude Code | Keychain item `Claude Code-credentials`, else `~/.claude/.credentials.json`; `~/.claude.json` | The access token, to `api.anthropic.com` |
| Codex | `~/.codex/sessions/**/rollout-*.jsonl` and `~/.codex/auth.json` | When those logs are over 10 minutes old: the access token and account id, to `chatgpt.com` |
| Grok Build | `~/.grok/auth.json` | The session token and user id, to `cli-chat-proxy.grok.com` |

Tokens are never refreshed, written, or logged, and travel only over HTTPS.
An expired sign-in shows as `unavailable` until you use that agent again.
Requests to one vendor are at least two minutes apart, with longer waits
after a rate limit.

The usage plugin, part of the starter distribution, runs
`phux agent usage --publish` when a client attaches or an agent changes
state. That shares the numbers with every client allowed to see the whole
server, including paired devices such as a phone. It shares an anonymous
account fingerprint, never an email or a token.

