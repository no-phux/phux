# phux Agent Tools Demo Plugin

A local plugin package for trying the agentic plugin surface without touching
your real `~/.config/phux/config.toml`. From the repository root:

```sh
export XDG_CONFIG_HOME="$PWD/examples/plugins/agent-tools/config"

cargo run -q -p phux -- config plugins          # com.phux.demo.agent-tools 0.1.0 (enabled)
cargo run -q -p phux -- config run com.phux.demo.agent-tools inspect
cargo run -q -p phux -- config run com.phux.demo.agent-tools inspect --json
```

`just plugin-demo` runs the same discover/validate/run sequence. Every action
in `phux-plugin.toml` runs the same way: `list-integrations`,
`validate-integrations`, `status-integrations`, `link-integration`,
`unlink-integration`, `detect-agents`, `smoke-integrations`, `launch-bench`,
`list-bench`, `drive-bench`, and `smoke-agent-wrap`. `--json` wraps an action's
stdout in the stable action result schema (argv, cwd, exit code, stderr,
duration).

## Agent bench

The `agent-bench` workspace profile composes the inspection actions with
runnable bench actions across four roles. `launch-bench` creates one phux
session per role and writes a role/session state table, `list-bench` prints
it, and `drive-bench` sends keys to the selected role. Roles launch as plain
shells, not real agent binaries. Customize with `PHUX_AGENT_BENCH_ROLES`,
`PHUX_AGENT_BENCH_PROFILE`, `PHUX_AGENT_BENCH_STATE`, `PHUX_AGENT_BENCH_ROLE`,
and `PHUX_AGENT_BENCH_KEYS`.

## Integration templates

`integrations/*.toml` are sample manifests for terminal-native agents
(`codex`, `claude-code`, `gemini-cli`, `grok`, `generic-shell-agent`). Each
declares an id, display name, version, status, capabilities, launch command,
link-state policy, opt-in detection command, optional `required_executables`
(a template is listed only when all are on `PATH`), and session identity
policy.

- `link-integration` / `unlink-integration` write only plugin-local state under
  `state/integrations`; they never install or run the agent. They default to
  Codex and Claude Code; override with `PHUX_AGENT_PACKAGE` or
  `PHUX_AGENT_PACKAGES`. `PHUX_CODEX_SESSION_ID` / `PHUX_CLAUDE_SESSION_ID`
  record a native session identity; otherwise the phux target is recorded.
- `status-integrations` reports `missing`, `current`, or `outdated` against the
  checked-in template version.
- `detect-agents` probes nothing unless `PHUX_AGENT_TOOLS_DETECT=1`;
  `PHUX_AGENT_TOOLS_PATH` overrides the search path for tests.
- `list-integrations` and `validate-integrations` are pure fixture checks.
- `smoke-integrations` exercises validate, list, link, status, fake-CLI
  detection, and unlink in a temporary state directory.

## Automatic agent identity

`scripts/phux-agent-wrap.sh` wraps an agent command so its pane carries a
`phux.agent/v1` record (ADR-0040) from launch until exit. The sidebar and
`phux agent list` prefer that record over the OSC-title heuristic, which
remains as a fallback for unwrapped agents. Each template's `[launch]` runs its
agent through the wrapper:

```toml
[launch]
command = ["sh", "${PHUX_PLUGIN_ROOT}/scripts/phux-agent-wrap.sh", "--name", "claude", "--kind", "claude", "--", "claude"]
working_directory = "workspace"
```

`phux launch <integration>` ([ADR-0042](../../../docs/adr/0042-launch-executor.md))
resolves that command from an enabled plugin, expands `${PHUX_PLUGIN_ROOT}`,
and spawns a pane. The server injects `PHUX_TERMINAL_ID` into every pane it
spawns, so the wrapper self-targets with no configuration:

```sh
phux launch --list
phux launch claude-code
phux launch codex -- --model o3   # extra args pass through
phux launch codex --print         # print the argv without spawning
```

To start a wrapped agent in an existing pane, run the wrapper directly (or
alias it), passing `--target` / `PHUX_AGENT_TARGET` when `PHUX_TERMINAL_ID` is
not set. For plain `claude` outside phux, `phux agent install-claude` installs
the product shim with lifecycle hooks instead.

The wrapper resolves its pane once and never falls back to the focused pane,
because the exit-time clear would race focus and could delete a sibling
agent's record; with no target it writes nothing but still launches the agent.
Record writes are best-effort and argv-only (no `eval`). `PHUX_AGENT_PHUX_BIN`
points at a non-`PATH` `phux`. `smoke-agent-wrap` checks targeting, event
payloads, failure and command-not-found records, and signal forwarding against
a stub `phux`, with no server.

### Lifecycle state

Interactive agents declare only identity and leave lifecycle to provider
screen/title rules. Grok's wrapper passes `--stream-single-turn` only for
one-turn modes (`-p` / `--single`, `--prompt-file`, `--prompt-json`), whose
blank viewport would otherwise hide progress: it opens an AgentSession and
emits `session_start` and `prompt` (a character count), then `stop` on exit 0
(with a one-second `done` grace) and `session_end` with a reason on every
outcome, closing the exact child it opened. Interactive `grok` never opens the
stream, which would pin it `working` for the whole session.
