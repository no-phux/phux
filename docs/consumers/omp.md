---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-09-30
---

# Native Oh My Pi integration

**TL;DR.** Build `integrations/omp` and load it with
`omp -e /absolute/path/to/integrations/omp` for CLI-backed terminal and agent
tools. The extension preserves branch-local targets without attaching or
changing focus. Inside phux it reports native AgentSession lifecycle for its
hosting pane only, without starting model requests. Use the shell tool for
ordinary one-shot commands.

## Install and load

The extension targets OMP 17.1.2 and requires Bun 1.3.14 or newer. It uses
`ExtensionAPI` from `@oh-my-pi/pi-coding-agent`, native `pi.registerTool`, and
the host's plain JSON Schema `TSchema` alternative, not the Pi SDK or a
compatibility shim. Development SDK dependencies are pinned; the installed
bundle has no npm runtime dependencies.

From a phux checkout:

```sh
bun install --cwd integrations/omp
bun run --cwd integrations/omp build
omp -e /absolute/path/to/phux/integrations/omp
```

`package.json#omp.extensions` points at `dist/index.js`. Build before loading a
checkout: OMP must find that manifest entry. The Bun bundle embeds the shared
integration runtime and tool definitions, so a copied or packed artifact does not
need `../runtime`, the repository, or development `node_modules` at runtime.

To persist the local installation in OMP's plugin state:

```sh
omp plugin link /absolute/path/to/phux/integrations/omp
```

Alternatively add the absolute package directory to `extensions:` in your OMP
configuration. Do not load both a source file and its bundle. The integration
never changes user configuration itself. This package is private and locally
installable, not published to npm. Packaging is supported with
`npm pack ./integrations/omp`; `prepack` builds the shipped entry.

OMP's authoritative host references are
[extension authoring](https://github.com/can1357/oh-my-pi/blob/main/docs/extensions.md),
[extension loading](https://github.com/can1357/oh-my-pi/blob/main/docs/extension-loading.md),
and [plugin installation](https://github.com/can1357/oh-my-pi/blob/main/docs/plugin-manager-installer-plumbing.md).
These upstream documents may describe newer versions; the package pins the SDK
used by its native loader smoke check.

## Configuration and identity

Configuration is inherited from the environment at extension load; restart or
reload the extension after changing it.

| Variable | Meaning |
|---|---|
| `PHUX_BIN` | Executable path; defaults to `phux`. An executable only, never shell arguments. |
| `PHUX_SOCKET` | Explicit local Unix socket; otherwise normal CLI socket/profile resolution applies. |
| `PHUX_PROFILE` | Normal phux CLI profile selection, inherited unchanged. |
| `PHUX_TARGET` | Optional explicit default `@N` or `host/@N` control target. Never inferred from focus. |
| `PHUX_TERMINAL_ID` | Hosting pane identity injected by phux; numeric IDs are normalized to `@N`. Do not repurpose this as a sibling target. |

Malformed hosting identity fails extension loading rather than silently disabling
the self-input guard. When OMP runs outside phux, no hosting pane exists to guard.
Do not remove a real hosting identity from the environment.

Target precedence is an explicit tool argument, the current branch's selected
terminal, then `PHUX_TARGET`. Missing targets fail rather than using global focus.
Successful `phux_create` selection is persisted in the session as a
namespaced custom entry; `phux_spawn` returns a target without changing selection.
Selection is reconstructed from the current branch on
each call, so restart/resume and tree navigation use branch history, not a global
variable. Forks inherit only entries in their ancestry. A create finishing after
a session switch or branch/tree navigation still returns the created terminal,
but does not select it in the newly active context; use its returned target
explicitly. Concurrent creates on one branch select in completion order.

## Tools and human handoff

The adapter exposes the complete shared tool surface:

- Discovery: `phux_list`, `phux_panes`, `phux_status`, `phux_runtime_info`.
- Creation: `phux_create`, `phux_spawn`.
- Terminal control: `phux_snapshot`, `phux_send_keys`, `phux_paste`, `phux_run`, `phux_wait`.
- Agent/resource control: `phux_agent_prompt`, `phux_agent_wait`, `phux_resource_wait`.

Use snapshots and explicit waits rather than repeatedly typing or guessing agent
readiness. Use `phux_paste` for literal text and `phux_send_keys` for actual key
chords. Agent prompts and shell commands are different operations. The build copies
the canonical [native-tools skill](../../.agents/skills/using-phux-tools/SKILL.md)
into `skills/using-phux-tools/SKILL.md`; OMP discovers that conventional package
directory for explicit `-e` directories and installed plugins. OMP 17.1.2 has no
`omp.skills` manifest field. The generated copy is ignored in Git and included in
the package. The [using-phux skill](../../.agents/skills/using-phux/SKILL.md) explains
the underlying CLI workflow; this package does not maintain a divergent copy.

Human commands:

- `/phux-status` reports connectivity and this branch's selected/hosting identities.
- `/phux-attach SESSION` prints a shell-quoted command to run in **another** terminal.
  It never executes that command, attaches, resizes a pane, or changes focus. Use
  `phux_list` to obtain the session name; a control target such as `@18` is not a
  session name. Without a name it displays usage, never a focus-dependent command.

## Safety, concurrency, and results

The neutral runtime invokes phux with an argv array, not a shell command line.
Control targets must be direct terminal IDs, including host-qualified IDs; session
and window aliases cannot bypass the hosting-pane input guard. `phux_run`,
`phux_send_keys`, `phux_paste`, and `phux_agent_prompt` refuse the hosting terminal.
Read-only observations of that terminal remain possible. The guard does not
sandbox shell commands: a permitted shell command can itself invoke other
programs. Terminal content is untrusted output, never extension policy.

Tools forward OMP's `AbortSignal` to the local subprocess runner. Cancellation
stops the local CLI invocation; it does **not** promise rollback, erase already
sent input, or kill a remote workload. Mutations are never automatically retried.
Check the terminal before retrying a cancelled or timed-out mutation. Short CLI
calls default to a 10-second local deadline; run/wait operations default to a
30-second CLI wait with a 5-second subprocess margin. Explicit timeout arguments
are documented by each tool's schema.

Independent terminals may be controlled concurrently. Serialize writes to the
same terminal and acknowledged agent prompts across the fleet, as the CLI's
acknowledged prompt lane is server-wide. Do not rely on completion order between
concurrent model tool calls. The adapter does not hold a global lock while waiting.
Read-only tools use OMP's `read` approval tier; mutations use `exec`.

Successful calls return bounded text content plus structured `details` from the
shared runtime, including targets, outcomes, and truncation information when
applicable. The shared text limit is 200 lines and 12 KiB. Failures return native
`isError: true` with bounded text and a structured error code/message and exit code
when available. CLI JSON error fields are retained when within the shared output
bound; oversized structured diagnostics are marked `cliErrorTruncated`.
Cancellation and local timeout remain distinguishable from malformed CLI responses
or executable failures. Error payloads omit raw argv and unbounded stderr, which
may contain prompts or secrets.

## Host-bound lifecycle

Only `PHUX_TERMINAL_ID` authorizes lifecycle reporting; selected targets and
`PHUX_TARGET` remain tool controls. Startup verifies the public terminal,
session and window projection, declaration provenance, and native child identity.
Missing, foreign or contradictory evidence disables reporting without mutation.
An existing child is adopted only with a matching `omp:<native-session-id>`
declaration, provider and native ID. Declarations contain identity only: never
`state` or `attention`. No prompt text, tool arguments or tool results are emitted.

Native `agent_start` reports `prompt`; tool execution reports names, call IDs and
success; aggregate `agent_end` reports `stop` unless `willContinue` is true.
Neither `turn_end` nor continuation-capable `session_stop` means completion.
Same-ID transcript reload and tree navigation do not open duplicate children.
Native ID changes retire the exact old child before binding a new one. Queued
operations carry captured IDs and generations; native-ID changes invalidate old
work. Same-ID navigation preserves active loop/tool completion. OMP 17.1.2 drains
old loops before switch/new events, but not before branch/tree events. If a native
ID changes through branch/tree during an active loop, subsequent activity reporting
is disabled until extension restart because aggregate events contain no session
ID; identity rotation and shutdown still proceed. This avoids falsely settling a
new session with a late old-loop event.
Every activity write checks current host ownership and addresses the exact child,
not the parent selector. A replacement child is never closed or relabeled.
Shutdown emits `session_end`, closes that child and clears only its matching
declaration. UI asked-state and fleet-context injection are not implemented;
existing detector rules remain authoritative for inferred UI state.

Reporting is best effort and independent of tools. CLI commands are bounded to
250 ms; aggregate shutdown is bounded to 1.2 seconds, below OMP's 2-second callback
cap. Any uncertain failure disables the reporter for this extension instance;
mutations are never retried. A timeout can leave a declaration or child behind;
inspect ownership rather than assuming rollback.

## Native loader smoke check

After dependency setup and build:

```sh
bun run --cwd integrations/omp typecheck
bun run --cwd integrations/omp test
bun run --cwd integrations/omp build
bun run --cwd integrations/omp smoke:load
# Optional: verified scratch binary, never the installed production binary
bun run --cwd integrations/omp smoke:lifecycle /absolute/scratch/path/to/phux
```

The script packs the package, extracts that artifact into a temporary directory,
launches a child Bun with isolated HOME/config/data/cache/session roots,
and invokes the pinned OMP discovery and extension loader. It verifies
all 14 tools, both commands, package skill discovery, host JSON Schema validation,
hosting-pane refusal, AbortSignal propagation, branch restoration, and a
create/tree-navigation race. A tiny
local CLI fixture supplies deterministic creation responses; no real phux socket,
production binary, model call, provider credential, or global user setting is used.
The temporary files are removed on completion. This proves native package loading
and adapter boundaries, not live-server behavior; real CLI semantics are covered
by the shared runtime's checks and isolated-server integration verification.

The optional lifecycle smoke packs the bundle, starts an owned disposable server
on an explicit temporary socket, and uses the locked SDK's real session manager
and `newSession`, `switchSession` and `branch` methods to verify automatic native
navigation events, same-ID reload, rotation and sibling isolation. Activity and
startup/shutdown events are scripted through the real runner, not model-generated;
this is not a claim of automatic model-loop timing coverage. HOME, XDG, profile,
credentials, tokens and TLS settings are isolated; no model/provider call is made.
The server and temporary roots are removed on completion.
