# examples/agents — driving phux end to end from the CLI

Runnable, self-documenting scripts that show an agent operating phux
**purely through the `phux` binary** — no client library, no TTY, no
tmux. The CLI *is* the agent surface (ADR-0022); anything that can spawn
a process and read JSON can drive a real terminal this way. The placed-fleet
example extends the single-pane loop into launch, topology, concurrent bounded
watching, and advisory human attention.

The introductory `01`–`04` scripts and `agent_loop.py` stand up their own
throwaway server on a private socket (see [`lib.sh`](./lib.sh)). The fleet
example is intentionally production-shaped: it uses the caller's normal socket,
creates a named session, and leaves that fleet running. Set `PHUX_SOCKET` to a
private server when experimenting.

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
| [`agent_loop.py`](./agent_loop.py) | all of the above | The same loop as code: subprocess calls + JSON parsing, no phux library. |
| [`orchestrate-placed-fleet`](./orchestrate-placed-fleet) | `new`, placed `launch`/`spawn`, `move-pane`, `swap-pane`, concurrent `watch` | A small placed fleet; bounded event subprocesses; blocked-ask surfacing; human `C-a q` / `C-a Q` guidance without moving focus. |
| [`orchestrate-placed-fleet-mcp.py`](./orchestrate-placed-fleet-mcp.py) | matching `phux_*` MCP tools | The same fleet through JSON-RPC stdio; separate adapters allow bounded watches to run concurrently. |

## Running them

```sh
bash examples/agents/01-ls-and-snapshot.sh
python3 examples/agents/agent_loop.py
PHUX_BUILDER_INTEGRATION=codex PHUX_REVIEWER_INTEGRATION=claude \
  examples/agents/orchestrate-placed-fleet
# With phux already running and phux-mcp on PATH:
python3 examples/agents/orchestrate-placed-fleet-mcp.py
```

They locate a `phux` binary in this order: `$PHUX`, then a `phux` on
`PATH`, then `target/debug/phux` (building it via `nix develop -c cargo
build -p phux` if missing — the dev shell provides the zig toolchain
libghostty-vt needs). The first build can be slow; subsequent runs reuse
it.

Only the bash scripts depend on `bash` + `python3` (used purely to parse
JSON in the output-extraction snippets). `phux` itself needs neither. The fleet
script expects two integration ids from `phux launch --list`; override them with
`PHUX_BUILDER_INTEGRATION` and `PHUX_REVIEWER_INTEGRATION`.

The deterministic gate substitutes a fake `phux` and needs no server or agents;
the live gate starts a real isolated server and shell panes:

```sh
just agents-fleet-smoke
just agents-fleet-live
```

The fake gate asserts complete argv/control flow, watch bounds, ask rendering,
and absence of focus/input-authority commands. The live gate dogfoods real
placement, insert/move/swap, and the real watch/ask event path without
external paid agents. To additionally try installed Claude/Codex binaries on the
private server, opt in explicitly:

```sh
PHUX_DOGFOOD_REAL_AGENTS=1 just agents-fleet-live
```

That command may invoke configured account/API behavior; it is never run by CI.

## Two gotchas the scripts bake in

- **`--socket` is a per-subcommand flag** and must precede a verb's
  trailing positional args (`phux send-keys --socket P NAME KEY...`, not
  `phux --socket P send-keys ...`). The `phux()` wrapper in `lib.sh`
  inserts it in the right place.
- **`wait --until` matches the echo of the command you just typed**, not
  only its output. Match on text that appears *only* in output — the
  examples wait on a value the program *computes at runtime*
  (`result=49`), which never appears in the command's own source.

## See also

- [`../skills/phux-terminal/SKILL.md`](../skills/phux-terminal/SKILL.md) —
  when to reach for phux over a one-shot shell, and the `run`/`wait`
  surface in prose.
- [`../skills/phux-agent-cli/SKILL.md`](../skills/phux-agent-cli/SKILL.md) —
  the JSON-driven CLI-wrapping playbook that points at these scripts.
- [`../../docs/consumers/tui.md`](../../docs/consumers/tui.md) — the full
  CLI shape.
- [`../../docs/consumers/pi.md`](../../docs/consumers/pi.md) — package setup
  when Pi should drive the same CLI surface.
- [`../../docs/adr/0022-tool-for-agents.md`](../../docs/adr/0022-tool-for-agents.md) —
  why the CLI is the agent surface.
