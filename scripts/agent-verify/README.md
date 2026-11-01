# Agent-state verification

Measures which evidence tier phux actually uses for a live agent, and how long
that state takes to show up. Screen is the floor. A closer source wins when
the agent (or its integration) reports one.

The server does not publish its internal rank. This harness reads only the
public CLI:

- `phux watch --json` for `agent_state` events (the API client)
- `phux agent show --json` for `state`, `sources`, and `agent_session`
- `phux resource show --json` for `program_status` (OSC 7501) and `agent_session`
- two `phux attach` clients on the same session, compared by the working glyph
  (`◐`) the TUI draws for that state

`agent_state` lines have no `ts_ms`. Time-to-state is the harness clock from
the stimulus (the stub's mark, the prompt, or spawn) to the watch line.

## Run it

From a checkout, on macOS or Linux, with the agents you care about already
logged in. The harness does not log in, does not install an integration into
your account, and does not send telemetry. It starts a private server
(`PHUX_PROFILE=agent-verify`, a private socket) and leaves your default server
alone. Real CLIs still see your home directory so they can use the logins
already there.

```sh
python3 scripts/agent-verify/run.py
```

`PHUX` or `--phux` selects the binary. Otherwise it uses `phux` on `PATH`,
then `target/debug/phux`.

One missing CLI is a single `not_installed` row. Codex has screen rules and
no lifecycle integration in this repo, so its integration-on row is
`integration_unavailable` and is not a measured session. A run that does not
happen is never filled in with a guess.

| Agent | Integration off | Integration on |
| --- | --- | --- |
| Claude Code (`claude`) | `PHUX_AGENT_REPORTING=0` | `--plugin-dir integrations/claude` |
| Codex (`codex`) | the CLI as installed | no integration shipped |
| OpenCode (`opencode`) | `PHUX_AGENT_REPORTING=0` | checkout plugin via a temporary `opencode.json` |
| pi (`pi`) | `PHUX_AGENT_REPORTING=0`, cwd outside this repo | cwd is this repo, so `.pi/settings.json` loads `integrations/pi` |

Off uses your existing install but turns reporting off for that process.
On points at the integration in this checkout without writing it into your
config. Each live run pastes `Reply with exactly the word pong.` and waits.

## Results

```text
./agent-verify-results/<UTC>/results.json
./agent-verify-results/<UTC>/results.md
./agent-verify-results/<UTC>/logs/<agent>-<on|off>/
```

`results.md` is the table: agent, integration, tier that won, time-to-state
p50 and max (milliseconds), whether the two attach clients and the watch
client agreed, and pass or fail. Logs are redacted (home directories and
token-shaped strings). The directory is gitignored.

Useful flags: `--agents claude,pi`, `--timeout 90`, `--out DIR`, `--home DIR`.

## What the tier names mean

| Tier | Public evidence |
| --- | --- |
| `stream` | `agent show` source `stream` agrees, or `resource show` `agent_session.state` does |
| `osc7501` | `program_status.active` matches and there is no live agent session |
| `screen` | `agent show` says the record was derived by the detection manifest |
| `unresolved` | the state is published, but those documents do not say which rung wrote it |

A hook (`phux agent report-state`) that matches the screen is reported as
`screen`. An explicit `phux agent set --state` is not labeled in the record;
if nothing weaker explains the state, the tier is `unresolved` (`declared` or
`hook`). The process rung is not published. Those limits are copied into
every `results.json`.

## CI

`python3 scripts/agent-verify/self_test.py` runs the same harness against
`stub_agent.py`, which emits each agent's screen signal and, in integration
mode, the session-open / emit sequence the real integrations use. It does
not call Claude, Codex, OpenCode, or pi. Real numbers come from a Mac run of
`run.py`.
