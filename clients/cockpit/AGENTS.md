# Phux Cockpit Product Contract

## Immediate Product

Build the most elegant, immediate, reliable native Phux terminal for macOS.
Terminal and TUI-agent workflows are the proving ground, not a disposable
prototype. Input, rendering, focus, process lifecycle, accessibility, and
failure recovery must feel native and exceptionally polished.

`libghostty-vt` is the terminal engine. Phux Cockpit owns the native command
surface, client projection model, interaction, and presentation. Phux owns
durable work identity, execution authority, applied Run policy state, ordered
evidence, artifacts, and shared attention state. Cockpit may author product and
formation policy, but Phux records and enforces the selected policy. Durable
local work uses local Phux; Cockpit's direct local PTY path is explicitly
ephemeral and never grows a second coordinator.

## North Star

Phux Cockpit is becoming a native control environment for directed machine
work. One person should eventually direct hundreds or thousands of agents,
runs, processes, and services while attending only to meaningful decisions.
Building and using agents belong to one work model; they are not separate
products.

Read [docs/PRODUCT_DIRECTION.md](docs/PRODUCT_DIRECTION.md) before changing
product structure, identity, navigation, or orchestration boundaries.

## Decision Rules

- Make today's terminal simpler and better; do not expose future architecture
  as premature fleet-management UI.
- Treat terminals, agents, and processes as provider-backed resources attached
  to stable product identities. Never make tab, pane, window, PID, or
  effect-key position the durable identity.
- Prefer native, direct, spatial interaction over dashboards, chat walls,
  generic cards, or configuration-heavy workflows.
- Design for progressive disclosure and exception-driven attention. More work
  must not create proportionally more things for the operator to watch.
- Preserve evidence and lineage from objective to run, session, and artifact.
- Keep local operation first-class; distributed execution must extend the same
  model rather than replace it.
- Do not fake detach, restoration, durability, or visibility when an underlying
  runtime seam is missing. Establish the seam and test its invariants first.

## Chrome

Every band, control, icon and gutter in the chrome traces to the Geist theme
pack's own token ladder or to the 4pt grid — a band is one default-register
control tall (40) and hosts small-register controls (32) with 4pt shoulders.
Read [docs/DESIGN_SYSTEM.md](docs/DESIGN_SYSTEM.md) before adding a number to
`src/app.native` or `workspace_projection.zig`; the `ts-chrome-parity` test in
`src/native_extension.zig` stops drift by running the toolkit's layout audit
over the real markup tree in every reachable chrome state and declared window
size.

The terminal GRID is not on that grid and cannot be: no integer number of rows
ever lands on a 4pt multiple. Snap the container, float the rows inside it, and
never derive a chrome spacing value from cell metrics.

## Rendering Evidence

`native automate screenshot` renders through the SDK's CPU reference renderer,
not through the real macOS rasterizer. It cannot see anything CoreText does,
which is why a 35% text-weight defect once survived three diagnoses. Never
conclude "the text renders correctly" from a screenshot. Read
[docs/RENDER_FIDELITY.md](docs/RENDER_FIDELITY.md) before writing any test that
claims to check what the terminal looks like.

## Running the App

```sh
./scripts/dev-run.sh
```

Builds this checkout and runs it as a real app that cannot be confused with
`/Applications/Phux Cockpit.app`: its own bundle id, its own process name
(`phux-cockpit-dev`, so `pgrep -x` and System Events target it and not the
installed copy), and its own config, layout and automation dropbox under
`.dev-run/`. Never hand someone a change to look at by telling them to open the
installed app — three days of bug reports once went to a build a week older
than `main` exactly that way. `./scripts/dev-isolation-check.sh` is the proof
and README explains the mechanisms.

## Regression Tests Must Be Shown Failing

A test that claims to guard a fix must be watched failing WITHOUT that fix,
before it is committed. This is not a formality. A regression test here once
passed against the exact bug it was written to catch — it allowed "a few more
frames" to settle, and the broken code needed exactly four for four panes,
inside the allowance. Reading it revealed nothing; only disabling the fix did.

So: remove the fix, run

```sh
scripts/guard-red-run.sh --record <name> --test "the zig test name" <path>
```

mark the test with `// GUARD: <name>` on the line above it, and say so in the
commit message. The script refuses to record anything it did not watch fail,
and it distinguishes a genuine red from a break that merely stopped the tree
compiling. `zig build test` then keeps the bookkeeping honest on every run.

Read [docs/GUARDS.md](docs/GUARDS.md) for the ritual, the two scripts, and —
more usefully — what the mechanism still does not prove.

## Quality Bar

Every change should reduce cognitive load, preserve input and lifecycle
correctness, remain keyboard-fast and pointer-natural, and keep the interface
calm under concurrency. If a feature adds another surface to monitor without
compressing operational complexity, it is pointed in the wrong direction.

## The Gate

```sh
./scripts/zig-build.sh test > /tmp/t.log 2>&1; echo "exit=$?"
```

Plain `zig build test` still works and is still judged the same way. The
wrapper exists because several worktrees share one machine, and it does three
things `zig build` will not: it gives this worktree its own Zig global cache
(sharing only the package directory, by symlink), it kills build runners
orphaned by a dead session in THIS tree before starting, and it refuses to run
when your shell is standing in a different checkout than the one it would
build. A held lock on the shared global cache was measured blocking a build
past 45 seconds while an isolated one finished in 3; `scripts/zig-cache-isolation-check.sh`
reproduces that A/B on demand. See phux-cockpit-2ml.11.

**Judge the run by the exit code.** Never by a log line. Nothing printed to
stdout or stderr is authoritative, and a green run has previously ended with a
line reading `failed command: .../test --listen=-`.

**Then read the verdict.** The run ends with a block naming what it compiled:

```
------------------------------------------------------------------
zig build test: PASS
  source root:   /path/to/the/worktree/you/think/you/are/in
  global cache:  /path/to/that/worktree/.zig-global-cache (worktree-private)
  phux provider: COMPILED AND TESTED (transport, host, provider, pointer)
  ...
------------------------------------------------------------------
```

**Check that `source root` names YOUR worktree.** A subagent's `zig build test`
once ran in a sibling worktree: the exit code was real, it just described
somebody else's code, and nothing else in the output distinguished it.
`global cache` says whether this run could be starved by a stuck build runner
somewhere else on the machine.

The verdict step depends on every test step, so it prints only when all of
them succeeded. **No verdict block means the run was not green**, whatever
else the output says.

**`source root:` must name the tree you edited.** On 2026-08-12 a subagent's
build silently ran in a sibling git worktree: the exit code was real, it just
described someone else's code, and nothing else in the output distinguished it.
Confirm `pwd` and `git rev-parse --show-toplevel` before treating any exit code
as evidence.

A verdict that says `PASS, INCOMPLETE` means `src/providers/phux/` was not
compiled, because the phux client FFI has not been built in this checkout. The
rest of the run is real; a change under `src/providers/phux/` is not verified by it.
The verdict prints the three locations it searched and how to fix it. Do not
report such a run as verifying a phux change.

The full phux-inclusive invocation, when the FFI lives outside the four
searched locations:

```sh
zig build test \
  -Dphux-enabled=true
```

From the repository root, `just cockpit-test` builds the FFI from the same
checkout and runs the shipping TypeScript graph plus retained native engine
regressions.

`-Dphux-enabled=true` additionally swaps the *app* graph onto the phux
provider. It is not needed just to compile and test `src/providers/phux/` —
discovering the FFI is enough for that.

See README.md, "Reading the result of `zig build test`", for the search order
and the reasoning.

## The Map

An ahead-of-time TypeScript + `.native` macOS app on the Native SDK fork
[`phall1/native`](https://github.com/phall1/native), pinned in `build.zig.zon`
by tarball sha. Cockpit is that fork's only real consumer, so breakage arrives
all at once at a pin bump; [docs/SDK_PIN.md](docs/SDK_PIN.md) has the checklist.

- `src/core.ts`, `src/app.native`, `src/windows/` — the one app coordinator and
  declarative chrome.
- `src/cockpit/` — native engine, topology, layout, session state, providers,
  and terminal runtime. `src/cockpit/native/` is the projection/paint boundary.
- `src/providers/` — `contract.zig` holds provider-neutral identity; `local/`
  runs PTYs in-process, `phux/` reaches a remote phux across an FFI ABI.
- `src/terminal/` — one `libghostty-vt` session per terminal, projected into a
  canvas grid. The emulator owns cell state, damage, scrollback and selection.
- `src/config/`, `src/tests/`.

**One geometry.** `layout.resolve()` is the single source of pane rects for the
painter, the hit-test widget tree and the PTY sizing pump. They were three
independent derivations once, and had already drifted.

**Two rasterizers.** On glass the AppKit host decodes the packet path and draws
with CoreText; `native automate screenshot` runs a CPU reference renderer that
never calls CoreText. See "Rendering Evidence" above.

### Entry points beyond the gate

<!-- PLACEHOLDER: scripts/dev-run.sh was in flight on 2026-08-12 and is the
     answer to "how do I run this". Add its invocation here once it lands,
     rather than guessing its flags. -->

| Script | Answers |
|---|---|
| `scripts/automate-smoke.sh` | does the real bundle come up, present through the packet path, and respond to driven input? |
| `scripts/drive-shell-ceiling.sh --want N` | how many concurrent shells does the shipped bundle reach before one is refused? |
| `scripts/host-raster-check.sh` | does the host's real CoreText rasterizer still ink glyphs as thickly as the pinned baseline? |
| `scripts/host-raster-compare.sh <ref>` | did an SDK change move glyph pixels between two commits that both shipped? |
| `scripts/check-sdk-pin.sh` | does README name the sha `build.zig.zon` actually resolves? |
| `scripts/build-automation-cli.sh` | builds the only `native` CLI that can drive this app; the npm one is fingerprint-refused and always will be. |

Check what binary a bug report is about before diagnosing it. On 2026-08-12
`/Applications` held 0.7.1 built three days earlier while `main` was 0.8.0, so
nothing merged in those three days had been in front of the person reporting.

## Evidence Rules

**An assertion you have not seen fail is not evidence.** Assert absent, act,
assert present. A merged and publicly retracted "automation input is broken"
finding came from an assertion that could not move: it counted
`role=textbox name="Terminal`, which only ever matches the SELECTED tab, so a
new tab could not shift it however perfectly the key was delivered. The
`expect_change` helper in `scripts/automate-smoke.sh` encodes the shape.

**Never compare against a counterfactual you authored.** The font-smoothing fix
was validated by hand-editing the SDK into a state no build was ever in; against
the real prior commit it moves zero pixels. Compare two things that shipped —
`scripts/host-raster-compare.sh` exists for exactly this.

**Live-app automation is serial-only** (`phux-cockpit-2ml.10`). The automation
dropbox is per-user rather than per-process and activation targets a process by
NAME, so a second bundle silently steals the channel, and a snapshot of a dead
instance returns an empty widget tree instead of an error. Assert
`publisher_pid` is the pid you launched. Guard with `pgrep -x <exename>` or
`pgrep -f "[p]attern"`; plain `pgrep -f` matches the shell running it, which is
fatal inside a wait loop and quietly wrong outside one.

**Measure constants, never pick them**, and keep the deriving command beside the
number. **Show every regression test failing first**: disable the fix, watch
red, restore, watch green, and report what you disabled.

Settled questions live in [docs/DECISIONS.md](docs/DECISIONS.md); reopen one by
adding information, not by re-litigating it.

**AGENTS.md and CLAUDE.md are independent files**, not symlinked, and they have
already drifted: this file carried the product contract, the rendering warning
and the test gate while CLAUDE.md was still scaffold placeholders. CLAUDE.md is
the build, the map and the working rules. Mirror substantive changes into both.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:970c3bf2 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   bd dolt push
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->

<!-- BEGIN BEADS CODEX SETUP: generated by bd setup codex -->
## Beads Issue Tracker

Use Beads (`bd`) for durable task tracking in repositories that include it. Use the `beads` skill at `.agents/skills/beads/SKILL.md` (project install) or `~/.agents/skills/beads/SKILL.md` (global install) for Beads workflow guidance, then use the `bd` CLI for issue operations.

### Quick Reference

```bash
bd ready                # Find available work
bd show <id>            # View issue details
bd update <id> --claim  # Claim work
bd close <id>           # Complete work
bd prime                # Refresh Beads context
```

### Rules

- Use `bd` for all task tracking; do not create markdown TODO lists.
- Run `bd prime` when Beads context is missing or stale. Codex 0.129.0+ can load Beads context automatically through native hooks; use `/hooks` to inspect or toggle them.
- Keep persistent project memory in Beads via `bd remember`; do not create ad hoc memory files.

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.
<!-- END BEADS CODEX SETUP -->
