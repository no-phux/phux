# phux for Muse

A CLI operating guide and shell helpers for headless agents. This is not
a Muse plugin or lifecycle adapter; it needs only shell execution, `phux`
and `jq`. It does not install software or change agent configuration.

## Use from a source checkout

From `integrations/muse`, read `SKILL.md` and source the harness-neutral
library at `../../.agents/lib/phux-agent.sh`. Keep the checkout intact:
this private package is not a standalone npm distribution.

```sh
. ../../.agents/lib/phux-agent.sh
pane=$(phux_spawn muse-build)   # creates an ephemeral session, prints @N
phux_run "$pane" 'printf hello' # command output and the command's exit status
phux_snap "$pane"
phux_kill "$pane"             # only this exact pane, no confirmation prompt
```

`phux_spawn` uses the CLI's create-only `new` path, which can start a server
on demand. All other helpers, including `phux_init` and `scripts/setup.sh`,
are read-only with respect to server startup: a missing server stays
missing. Choose the server deliberately with `PHUX_SOCKET` or `PHUX_PROFILE`.
Source builds must never target the installed release's production socket.
See [development](../../docs/development.md#running-the-result) and
[installation](../../docs/install.md).

Helpers use exact **local** `@N` targets and reject the hosting pane from
`PHUX_TERMINAL_ID`. For federation, interactive input, agent lifecycle,
mail and path claims, use the raw CLI described in `SKILL.md` and the
[agent guide](../../docs/agents.md). There are no shell aliases for those
operations here.

## Checks

```sh
npm run gates                 # syntax, structural and mocked CLI behavior tests
./scripts/setup.sh            # check dependencies and existing server, no writes
./scripts/smoke.sh /path/to/test.sock
```

The smoke test requires an explicit **isolated** server socket argument,
overriding any inherited `PHUX_SOCKET`.
It creates one uniquely named ephemeral session, runs a command, observes
its marker with wait and snapshot, and kills its exact pane on success,
failure or a handled signal. It never stops the server. Do not point it
at the production server. A marker on screen may include command echo;
the `run` result, not that marker alone, establishes command completion.

The shared library keeps variables and traps inside subshells. Timeouts
are positive integer seconds (default 20; `PHUX_AGENT_TIMEOUT` overrides
it); zero/unbounded waits are refused. CLI failure diagnostics go to stderr,
and `phux_run` retains failed child output and exit status. A timeout
stops observation, not the process: never resend uncertain input.

`CHANGELOG.md` and `package.json` track this integration independently of
the phux release. CI runs these gates in the integrations lane, including
when the shared shell library changes.
