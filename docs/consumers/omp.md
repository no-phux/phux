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

The extension targets OMP 18.6.1 and requires Bun 1.3.14 or newer. It uses
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
directory for explicit `-e` directories and installed plugins. OMP 18.6.1 has no
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

An awaited, payload-free `before_agent_start` guard establishes which native loop
may emit activity; it never reads or injects prompt text. Native `agent_start`
reports `prompt`; tool execution reports names, call IDs and success. Aggregate
`agent_end` reports `stop` unless `willContinue` is true. A delivered continuation
allows another start, with or without the before hook, without inventing completion.
Neither `turn_end` nor continuation-capable `session_stop` means completion.

Passive native `tool_approval_requested` / `tool_approval_resolved` hooks track
session ID, tool-call ID and tool name. Requests emit metadata-free permission
notifications: no question, answer capability, optional reason or tool input is
captured. Concurrent approvals remain blocked until the last matching resolution;
tool-start records cannot clear that block. Last resolution emits a stream-only
working assertion, not a new prompt or completion. Declarations still never contain
state or attention. Other UI questions and fleet-context injection are not covered.

OMP 18.6.1 delivers generic activity concurrently and detaches aggregate extension
notifications. Navigation/abort does **not** drain their delivery. OMP 18 removed
the FIFO subscriber gate that made the received aggregate a barrier in 17.x; the
lifecycle smoke's delayed-tool case still observes the received aggregate trailing
a held-up earlier tool delivery on the pinned SDK, but that ordering is now
emergent rather than an SDK contract, so re-verify it on every OMP bump.
OMP 18 also runs `before_agent_start` for queued steering and follow-up messages
delivered mid-run, and may repeat it while preparing one prompt. The event carries
nothing that distinguishes those from a new prompt, but the running loop absorbs
a queued delivery without an `agent_start` of its own. So while OMP reports the
session streaming (`ctx.isIdle()` false), a guard during prepared/active work is
a no-op: typing into a busy OMP keeps reporting, and a genuinely overlapping loop
still falls back when its own `agent_start` arrives. A guard while the session
reports idle cannot be an absorbed delivery and falls back. Residual cost: if an
earlier extension holds the old loop's `agent_end` past a new prompt's guard,
that end reports `stop` before the new start falls back, so `done` can show
briefly before detection takes over.
A collaboration guest (`omp join`) mirrors starts without the before hook and
lands in the same fallback. An overlapping start, an idle-session guard during
prepared/active work, an unguarded start, or navigation during
prepared/active/continuing work therefore retires the
verified exact child and remains **declaration-only** until extension restart.
This includes same-ID transcript reload, whose abort can discard completion.
Same-ID tree navigation preserves the existing loop; idle reload is idempotent.
Native ID rotation preserves the proven declaration while replacing its native
identity. Queued writes retain captured IDs/generations. Missing or contradictory
ownership stops mutation; replacement children are never closed or relabeled.

Declaration-only fallback creates no silent authoritative child. Removing the
owned child restores detector eligibility for working and approval-blocked state.
Approval hooks cannot reopen a child in fallback, and neither reload nor later
starts restore trust. Normal shutdown ends/closes the exact child and clears only
the matching declaration.

**Cold bootstrap policy:** on this instance's first native `session_start` only,
an exact `name: omp`, `kind: omp` record with absent/null session ownership may be
initialized when the canonical host/session/window are proved and `agent_session`
is explicitly null. This applies equally to a manually written unbound identity;
it does **not** prove that a detector authored the record. Screen-rule evidence
cannot establish publisher provenance. Only the known identity fields and optional
canonical observational state are accepted; non-null attention, unknown fields,
duplicate/malformed records and every existing child are refused. Empty-string
session ownership is not unbound. Initialization writes identity only, discarding
observed state/attention, then re-verifies before opening the child.

This permission is never reused for navigation, reload or recovery. A matching
owned declaration still requires strict provider/native-ID/exact-child adoption
proof; another owner is never replaced. Missing record plus explicit null child
retains the existing fresh-binding path. Approval receipt tombstones are forgotten
at the received aggregate barrier: the SDK wrapper awaits their resolution before
its tool completes, so later guarded loops may safely reuse tool-call IDs.

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
on an explicit temporary socket, and uses the locked SDK's real session manager,
runner and tool wrapper. A deferred UI always denies inert tools, demonstrating
blocked state across concurrent approvals and honest resolution without executing
action bodies. Earlier extension handlers deliberately delay before/start/tool/end
delivery; the SDK's actual detached aggregate path, new/resume/fork, same-ID reload,
exact-child retirement and restored detector blocking are exercised. Normal idle
navigation and continuation are also covered, as is a queued follow-up that an
inert local transport's running loop absorbs through the before hook. No live
model reasoning is claimed. The delayed-tool case checks emergent ordering, not
an SDK contract, and fails intermittently on 18.6.1 (about one run in six).

An owned inert `omp.js` process supplies kernel identification and a fixed approval
screen. Startup initializes its unbound identity without predeclaration; approval
and causality scenarios do not call `agent set` as fixture setup. A separate owned
pane runs the actual pinned OMP CLI entrypoint with the packed extension. A local
fixture registers an inert custom model, records the automatic native
`session_start`, and fails if any provider transport is invoked. Its native ID must match the exact host child, while the
selected sibling remains untouched. A diagnostic wrapper records actual CLI argv,
stdout/stderr and exit; it does not supply lifecycle events or identity metadata.

HOME, XDG, profile, sessions,
credentials, tokens and TLS settings are isolated. No actual provider credentials
or model/service calls are used. Owned servers and temporary roots are removed.
