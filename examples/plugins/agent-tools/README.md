# phux Agent Tools Demo Plugin

This fixture is a first-class local package for trying the agentic plugin
surface without touching your real `~/.config/phux/config.toml`.

Run it from the repository root:

```sh
export XDG_CONFIG_HOME="$PWD/examples/plugins/agent-tools/config"

cargo run -q -p phux -- config plugins
cargo run -q -p phux -- config plugins --json
cargo run -q -p phux -- config run com.phux.demo.agent-tools inspect
cargo run -q -p phux -- config run com.phux.demo.agent-tools inspect --json
cargo run -q -p phux -- config run com.phux.demo.agent-tools list-integrations
cargo run -q -p phux -- config run com.phux.demo.agent-tools validate-integrations
cargo run -q -p phux -- config run com.phux.demo.agent-tools status-integrations
cargo run -q -p phux -- config run com.phux.demo.agent-tools link-integration
cargo run -q -p phux -- config run com.phux.demo.agent-tools status-integrations
cargo run -q -p phux -- config run com.phux.demo.agent-tools unlink-integration
cargo run -q -p phux -- config run com.phux.demo.agent-tools detect-agents
cargo run -q -p phux -- config run com.phux.demo.agent-tools smoke-integrations
cargo run -q -p phux -- config run com.phux.demo.agent-tools launch-bench
cargo run -q -p phux -- config run com.phux.demo.agent-tools list-bench
cargo run -q -p phux -- config run com.phux.demo.agent-tools drive-bench
```

Expected human output:

```text
com.phux.demo.agent-tools 0.1.0 (enabled)
```

The action prints the boundary the plugin system is meant to keep:

```text
phux plugin demo
core=stable terminal/session host
plugin=agentic workflow package
plugin_id=com.phux.demo.agent-tools
action_id=inspect
root=/path/to/phux/examples/plugins/agent-tools
```

`phux config run --json` wraps that stdout with the stable action result
schema, including argv, cwd, exit code, stderr, and duration.

The manifest also declares an `agent-bench` workspace profile. It composes
inspection/list/validation actions, static agent status records, pane roles,
and three runnable bench actions:

- `launch-bench` creates one phux session per role and writes a role/session
  state table.
- `list-bench` prints that state table.
- `drive-bench` sends keys to the selected role with `phux send-keys`.

The defaults are safe: roles launch as normal phux shell sessions, not real
agent binaries. Set `PHUX_AGENT_BENCH_ROLES`, `PHUX_AGENT_BENCH_PROFILE`,
`PHUX_AGENT_BENCH_STATE`, `PHUX_AGENT_BENCH_ROLE`, or `PHUX_AGENT_BENCH_KEYS`
to customize the fixture.

## Pane self-identification (`PHUX_TERMINAL_ID`)

The phux server injects `PHUX_TERMINAL_ID` into the environment of every
pane it spawns. Its value is the pane's own local wire id — the number in
the `@N` client selector (`TerminalId::local(N)`). Because the server sets
it automatically, a process running inside a pane can address itself on the
phux wire with zero configuration:

```sh
# From inside any spawned pane, target this same pane:
phux send-keys "@$PHUX_TERMINAL_ID" 'echo hi\n'
```

Agent tooling that records or supervises the pane it runs in (for example a
record wrapper) reads `PHUX_TERMINAL_ID` as its `@N` self-target rather than
requiring the id to be passed in. The same value is exposed to lifecycle
hooks as `PHUX_TERMINAL_ID`, so the pane is named identically across both
surfaces. Panes not spawned by phux (or addressed via a federation
`Satellite` id, which has no server-local `@N`) do not receive the variable.

## Integration templates

The `integrations/*.toml` files are sample manifests for terminal-native
agents that phux can launch, supervise, and report on through plugin actions.
They are intentionally local, documented packages rather than hidden product
magic. Codex and Claude Code are the first-party public packages; the Gemini
and generic shell records keep the fixture broad enough to test templates:

- `codex.toml`
- `claude-code.toml`
- `gemini-cli.toml`
- `generic-shell-agent.toml`

Each package declares a stable id, display name, package version, public
status, capabilities, launch command, link-state policy, opt-in detection
command, and session identity policy. Linking writes only plugin-local state
under `examples/plugins/agent-tools/state/integrations` by default; it does not
install or execute the agent CLI.

`status-integrations` reports package state as:

- `missing` when no local link state exists.
- `current` when the linked package version matches the checked-in template.
- `outdated` when a linked state file points at an older template version.

`link-integration` and `unlink-integration` default to the first-party Codex
and Claude Code packages. Override with `PHUX_AGENT_PACKAGE` or
`PHUX_AGENT_PACKAGES` to target a different package id. Native session identity
can be recorded without private credentials by setting the package-specific
environment variable, such as `PHUX_CODEX_SESSION_ID` or
`PHUX_CLAUDE_SESSION_ID`; otherwise the link records the phux session target.

Detection never probes the user's machine by default. To run it deliberately:

```sh
PHUX_AGENT_TOOLS_DETECT=1 \
  cargo run -q -p phux -- config run com.phux.demo.agent-tools detect-agents
```

Tests can avoid local installations by overriding the detection search path:

```sh
PHUX_AGENT_TOOLS_DETECT=1 \
PHUX_AGENT_TOOLS_PATH=/tmp/fake-agent-bin \
  cargo run -q -p phux -- config run com.phux.demo.agent-tools detect-agents
```

`list-integrations` and `validate-integrations` are pure fixture checks and
do not execute or inspect any local agent binaries.

`smoke-integrations` runs validation, list, link, status, fake-CLI detection,
unlink, and missing-status checks in a temporary state directory. It needs no
private credentials and leaves no plugin state behind.

## Automatic agent identity (phux-r82.11)

Plain `claude` / `codex` / `gemini` sessions never announce themselves, so the
sidebar and fleet views used to fall back to an OSC-title substring heuristic —
which false-positives on titles like `vim CLAUDE.md` and can never show a real
working/blocked state. `scripts/phux-agent-wrap.sh` fixes the identity half of
that: it wraps the real agent command so the pane writes a first-class
`phux.agent/v1` L3 record (ADR-0040) the moment the agent launches, and clears
it on exit.

Each `integrations/*.toml` declares a `[launch]` command that runs its agent
through the wrapper. For Claude Code:

```toml
[agent_identity]
name = "claude"
kind = "claude"

[launch]
command = ["sh", "${PHUX_PLUGIN_ROOT}/scripts/phux-agent-wrap.sh", "--name", "claude", "--kind", "claude", "--", "claude"]
working_directory = "workspace"
```

For the product path — where typing plain `claude` outside phux should create
and attach a self-identifying phux session — install the first-party shell shim
once per box:

```sh
phux agent install-claude
```

The fixture wrapper below remains the integration-template path used by
`phux launch`; the installed shim adds automatic entry plus Claude lifecycle
hooks (`working`, `blocked`, `done`, and `phux ask` notifications).

### Launch through `phux launch` (recommended)

phux **does** ship a launch executor: `phux launch <integration>`
(phux-ark7, [ADR-0042](../../../ADR/0042-launch-executor.md)) resolves a
template's `[launch]` command from an enabled plugin, expands
`${PHUX_PLUGIN_ROOT}` to the absolute plugin root, and spawns a pane
running it via `SPAWN_TERMINAL`. Because the server injects
`PHUX_TERMINAL_ID` into the spawned pane (phux-w7mj), the wrapper
self-targets with **zero extra config**:

```sh
phux launch --list          # codex, claude-code, gemini-cli, ...
phux launch claude-code     # opens a pane running claude through the wrapper
phux launch codex -- --model o3   # extra args pass through to the agent
phux launch codex --print   # resolve + print the argv without spawning
```

So with the plugin installed and enabled, `phux launch claude-code`
opens a **self-identifying** pane end-to-end — the wrapper writes the
`phux.agent/v1` record at launch and clears it on exit, no alias needed.
`working_directory = "workspace"` runs the agent in the directory you ran
`phux launch` from (the launch executor expands the wrapper path
absolutely, so it still resolves).

You can still activate the wrapper by hand — useful when you start an agent
in an existing pane rather than a fresh launch:

- **Wrap the command directly** in the pane where you start the agent:

  ```sh
  PHUX_TERMINAL_ID=<pane> \
    sh "$PHUX_PLUGIN_ROOT/scripts/phux-agent-wrap.sh" --name reviewer --kind claude -- claude
  ```

- **Alias it** in your shell rc so `claude` transparently runs through the
  wrapper, passing the pane target via `PHUX_AGENT_TARGET` / `--target`.

The sidebar's `agents` section and `phux agent list` already prefer the
`phux.agent/v1` record over the title heuristic
(`crates/phux-client/src/agent_meta.rs`), so a wrapped pane gets a first-class
identity while un-wrapped panes still fall back to the heuristic. The heuristic
remains as a fallback-only path for un-wrapped agents — it is not removed.

### Pane targeting is required (no focused-pane guessing)

The wrapper resolves the pane it is running in **exactly once, at launch**, and
reuses that same target for the exit-time `clear`. It does this from, in order:
`--target` / `PHUX_AGENT_TARGET`, else `PHUX_TERMINAL_ID` (used as the `@N`
selector). It deliberately **never** falls back to `phux agent set`'s
focused-pane default: focus moves freely and the exit-time `clear` fires much
later, so a focused-pane guess would race — in a multi-pane / fleet run it would
delete a *different*, still-running agent's record. If the wrapper cannot
resolve its pane it writes **nothing** (and prints one diagnostic to stderr) but
still launches the agent — a missing label is safer than a corrupted sibling.

The wrapper is otherwise best-effort and injection-safe: every value is passed
as its own quoted argv element to `phux` (never through `eval`/`sh -c`), and if
`phux` is missing or no server is up the record write fails silently so the
agent still launches. Set `PHUX_AGENT_PHUX_BIN` to point at a non-`PATH` `phux`
binary.

`smoke-agent-wrap` drives the wrapper against a stub `phux` and a fake agent,
asserting the launch write (`agent set @<pane> --name ... --kind ...`) and the
exit write pinned to the same pane (`agent clear @<pane>`), plus the safety case
that with no resolvable target the wrapper writes nothing at all. It needs no
server and leaves nothing behind:

```sh
cargo run -q -p phux -- config run com.phux.demo.agent-tools smoke-agent-wrap
```

### State is not fed live (yet)

The wrapper only observes the agent's launch/exit boundary, so it sets the
always-honest half — `name` + `kind` — and leaves lifecycle `state` unset
(`unknown`) unless you pass `--state` / `PHUX_AGENT_STATE`. A live
working/blocked feed would need a continuous signal updating the same record,
for example: the agent itself calling `phux agent set --state working|blocked`
on its own lifecycle transitions (e.g. via a phux hook or the agent's own
tool-call boundaries), or phux polling the ADR-0035 "asked" detector / OSC-133
prompt boundaries and writing the state field. Either path reuses this exact
record, so the sidebar's declared-state branch lights up the moment a state
feed exists — no consumer changes required.
