# @phux/pi

A pi extension that gives the agent
tools over phux terminals, and tells phux what the agent is doing.

```sh
pi install npm:@phux/pi
```

Or `pi install ./integrations/pi` from a trusted checkout. A trusted checkout
of this repository loads it through `.pi/settings.json` on its own.

`phux` must be on `PATH` with a running local server. The extension reads
`PHUX_SOCKET` to find a non-default socket and `PHUX_TERMINAL_ID`, which
phux sets inside each pane, to know its own pane.

## What it adds

Tools: list, create, spawn, launch, snapshot, send keys, paste, run, wait,
agent prompt and wait, resource wait, insert/move/swap pane, kill, signal,
tag, ask, collect events, rendered snapshot, status, runtime info, and
branch-local aliases. Commands for the person at the keyboard: `/phux` to
pick a target pane, `/phux-status` to check it, `/phux-attach` to print the
`phux attach` invocation to run in a real terminal.

At each user turn it appends a short fleet context (which panes exist and
what their agents are doing) unless `PHUX_CONTEXT_AWARENESS=0`. It reports
pi's own lifecycle to phux as an agent session stream, so the pane shows up
as `working`, `blocked`, or `done` in the sidebar and to `phux agent wait`.
`PHUX_AGENT_TRANSCRIPT=0` turns the transcript records off; `=full` adds
tool output.

Inside a phux pane it waits for the pane's mail (`phux mail wait`) and puts
each message in the conversation with `pi.sendMessage` as a `phux.mail`
follow-up that starts a turn when pi is idle. Nothing is typed into the pane.

## Safety

Input and destructive actions resolve the target first and refuse pi's own
pane and any selector broader than one exact pane. Kill and destructive
signals require `confirm: true` and check every member of a group before
destroying any. Nothing here emulates a verb the installed `phux` lacks; an
old binary fails the call rather than approximating it.

Input leases (`phux take`) are not exposed: a one-shot CLI call's lease ends
when the call does, so there is no durable form a tool could offer.

## Development

```sh
npm ci
npm run gates                        # typecheck, lint, tests, pack check
PHUX_PI_REAL_SMOKE=1 npm run smoke:real   # against a real phux, opt in
```

Releases publish to npm as `@phux/pi` with provenance, tagged
`pi-extension-vX.Y.Z`.
