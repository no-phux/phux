# examples/agents

Runnable scripts that drive phux through the `phux` binary alone: no client
library, no TTY, no tmux. Anything that can spawn a process and read JSON
can drive a real terminal this way. The placed-fleet example extends the
single-pane loop into launch, topology, concurrent bounded watching, and
asking a person for attention.

The introductory `01`-`04` scripts and `agent_loop.py` stand up their own
throwaway server on a private socket (see [`lib.sh`](./lib.sh)). The fleet
example uses the caller's normal socket, creates a named session, and leaves
that fleet running; set `PHUX_SOCKET` to a private server when
experimenting.

## The loop these examples teach

```
loop:
  READ  the screen     ->  phux snapshot --json   (side-effect-free)
  DECIDE what to do     ->  parse `lines` / `cursor`
  ACT   send input      ->  phux send-keys ...     (or phux run for a discrete command)
  WAIT  for the effect  ->  phux wait --until / --idle   (bounded by --timeout)
```

`run` collapses ACT+WAIT into one call for discrete commands (it returns
the exit code); drop to `send-keys` + `wait` for REPLs, TUIs, prompts, and
backgrounded work where there is no single "command done" moment.

## The scripts

| File | Verbs shown | Teaches |
|---|---|---|
| [`01-ls-and-snapshot.sh`](./01-ls-and-snapshot.sh) | `ls`, `ls --json`, `snapshot`, `snapshot --json` | The two side-effect-free reads: enumerate sessions, read a pane. |
| [`02-run-and-exit-codes.sh`](./02-run-and-exit-codes.sh) | `run`, `run --json` | Discrete commands; exit-code mirroring; `&&`/`||` chaining. |
| [`03-send-keys-and-wait.sh`](./03-send-keys-and-wait.sh) | `send-keys`, `wait --until`, `wait --idle`, `--timeout` | Structured input; blocking on a condition; driving a REPL. |
| [`04-read-act-wait-loop.sh`](./04-read-act-wait-loop.sh) | all of the above | The full read+act+wait loop against an interactive prompt. |
| [`05-program-status.sh`](./05-program-status.sh) | `run`, `agent list`, `agent explain`, `resource show` | A program reports its own state with OSC 7501. No hooks. |
| [`agent_loop.py`](./agent_loop.py) | all of the above | The same loop as code: subprocess calls + JSON parsing, no phux library. |
| [`orchestrate-placed-fleet`](./orchestrate-placed-fleet) | `new`, placed `launch`/`spawn`, `move-pane`, `swap-pane`, concurrent `watch` | A small placed fleet; bounded event subprocesses; blocked-ask surfacing; human `C-a q` / `C-a Q` guidance without moving focus. |

## Running them

`just examples-smoke` runs every introductory script, their negative-path
regressions, and a real-server check. CI also runs them in `just e2e` using
the same CLI as the rest of the test archive.

```sh
bash examples/agents/01-ls-and-snapshot.sh
bash examples/agents/05-program-status.sh
python3 examples/agents/agent_loop.py
PHUX_BUILDER_INTEGRATION=codex PHUX_REVIEWER_INTEGRATION=claude \
  examples/agents/orchestrate-placed-fleet
```

The introductory examples use `$PHUX` when explicitly set, otherwise this
checkout's `target/debug/phux`. Run `just build` first; they never silently
choose the installed binary or start a build. They use a private home and XDG
directories, discard inherited `PHUX_*` settings, select `/bin/sh`, and stop
and reap their server before removing its state. `bash` and `python3` must
be on `PATH`. An explicit `PHUX` selects a binary but still runs it privately.

Run every introductory example and negative-path regression against the same
checkout binary with:

```sh
bash scripts/examples-smoke.sh
```

The smoke also runs a fake Pi that only narrates success and proves the live
fleet harness rejects it. No credentials or paid agents are used by this gate.
`record-pi-live-fleet` requires `PHUX_DOGFOOD_REAL_AGENTS=1`; after Pi exits it
independently checks for distinct Claude/Codex children in the proof session
and working directory, sends each a fresh challenge, and verifies computed
response output from each exact pane. Transcript tokens alone never pass.

The fleet script takes two integration ids from
`phux launch --list` via `PHUX_BUILDER_INTEGRATION` and
`PHUX_REVIEWER_INTEGRATION`.

`agents-fleet-smoke` substitutes a fake `phux` and checks argv, watch bounds,
ask rendering, and the absence of focus/input-authority commands.
`agents-fleet-live` runs real placement, insert/move/swap, watch, and ask
against an isolated server with shell panes:

```sh
just agents-fleet-smoke
just agents-fleet-live
```

To also spawn installed Claude/Codex binaries (may use account/API quota;
never run by CI):

```sh
PHUX_DOGFOOD_REAL_AGENTS=1 just agents-fleet-live
```

## Two gotchas the scripts bake in

- **Put options before trailing command/key arguments** so `run` and
  `send-keys` do not send them as input. The `phux()` wrapper in `lib.sh`
  inserts `--socket` before positional arguments.
- **`wait --until` matches the echo of the command you just typed**, not
  only its output. Match on text that appears *only* in output: the
  examples wait on a value the program *computes at runtime*
  (`result=49`), which never appears in the command's own source.
  Formatted prompts and completion markers likewise avoid matching input echo.
  Only exit **124** means `wait` timed out; other failures remain errors.

## See also

- [`using-phux`](../../.agents/skills/using-phux/SKILL.md): the canonical
  CLI skill, including when to use phux and the full supervision loop.
- [`using-phux-mcp`](../../.agents/skills/using-phux-mcp/SKILL.md): the
  canonical MCP skill and live-schema workflow.
- Install the published copies with `npx skills add no-phux/skills`.
- [Driving terminals from scripts](../../docs/scripting.md),
  [coding agents](../../docs/agents.md), and
  [any program can report status](../../docs/any-program-status.md).
- [`integrations/pi`](../../integrations/pi) when pi should drive the same
  CLI surface.
