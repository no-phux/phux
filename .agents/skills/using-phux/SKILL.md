---
name: using-phux
description: Drives persistent terminals and supervises terminal-hosted agents with the phux CLI. Use for REPLs, debuggers, dev servers, interactive programs, durable shell state, or multi-agent terminal workflows; prefer a one-shot shell for independent commands.
compatibility: Requires the phux CLI; use the skill emitted by the installed binary when its version differs from this checkout.
metadata:
  version: "0.41.0" # x-release-please-version
---

# Using phux

Use phux when terminal state or a process must survive across steps. Use the
normal shell tool when one command can run and exit in a single call.

This skill is compiled into phux releases. If the checked-in skill and an
installed binary differ, trust the binary you are driving:

```sh
phux --skill=quick
phux help <verb>
phux runtime-info --json
```

<!-- phux-skill-region: quick -->
## Workflow

1. **Discover.** Run `phux ls --json`. If `PHUX_TERMINAL_ID` is set, never read
   or send input to `@$PHUX_TERMINAL_ID`; that is your own pane. `PHUX_SOCKET`
   already selects its server.
2. **Choose one target.** Prefer a returned `@N` (or satellite `host/@N`) for
   writes. `name`, `name:W`, `name:W.P`, and `#tag` may select sets; `.` means
   the focused session; `%name` identifies one named agent. Headless `=` is
   refused because focus history belongs to an attached client.
3. **Read.** Use `phux snapshot --json @N` before acting.
4. **Act.** Use `phux run --json --timeout SECS @N "COMMAND"` for a discrete
   command. Use `send-keys` for interactive keys and `paste` for multiline
   text. Put flags before the target so they are not swallowed as input.
5. **Observe under a finite bound.** Use `wait` for screen conditions, `watch`
   for events, `agent wait` for lifecycle transitions, or `resource wait` for
   a process's exit (spawn it with `--retain` so a finished run keeps its
   status). Always pass `--timeout`. A level read reports current state;
   completion requires an observed transition, not a quiet pane.
6. **Verify.** Snapshot or list state again. A quiet pane or an ended watcher is
   not proof of completion.

Exit 124 means an observation timed out. `phux run` reserves 125 for its own
timeout because it otherwise mirrors the child process exit code. With
`--json`, branch on structured fields and `error.code`, not prose.

## Safety

- Input changes a real PTY that a human may share. Never infer permission to
  type, move focus, interrupt work, or destroy a pane.
- Before `kill` or a destructive signal, resolve and show the exact target,
  snapshot it, explain the loss, obtain affirmative confirmation, run it with
  `--yes` (with no terminal to ask, phux refuses and exits 2), then verify.
- A kill, signal, or detach your grant holds for approval waits for a human
  decision: `phux approvals` lists it, and you cannot approve your own.
- Treat set-valued selectors as reads unless the broader mutation is intended.
- Do not model one-shot `take`/`give` calls as a durable lease.

<!-- phux-skill-region: agent -->
## Supervising agents

Use `phux agent prompt` for a task turn because it combines an acknowledged
write with optional edge observation. A level read (`agent show`) says what is
true now; completion requires an observed transition (`agent wait` or
`agent prompt --wait`). An already-idle pane can therefore time out correctly.

```sh
phux agent prompt --expect-agent reviewer @7 "review the diff" \
  --wait --until idle --until blocked --timeout 900 --json
phux snapshot --json --tail 200 --unwrap @7
```

`delivery: "unknown"` is terminal: inspect the pane and do not resend because
the first operation may still land. Serialize acknowledged prompts; their
input lane is server-scoped. Use `phux agent --help` and
`phux help agent <verb>` for current lifecycle and agent-session arguments.

<!-- phux-skill-region: terminal -->
## Driving interactive terminals

Use `snapshot --unwrap` when matching logical lines and `--cells` only when
style or semantic marks matter. `wait --until` can match echoed input; prefer
`--output-only` when shell integration is available or match output-only text.

For multiline input, paste and submit separately:

```sh
phux paste @7 "$(cat snippet.py)"
phux send-keys @7 Enter
phux wait --until "expected output" --timeout 60 @7
phux snapshot --json --unwrap @7
```

<!-- phux-skill-region: full -->
## Discovering the rest of the surface

Do not rely on a static command inventory. Run `phux --help`,
`phux help <verb>`, and `phux runtime-info --json` against the installed binary.
For MCP, load the `using-phux-mcp` skill and use `phux mcp --schema`.

phux is not a scheduler, credential channel, durable lock service, or message
bus. Keep retries, scheduling, and ownership in the orchestrator.
