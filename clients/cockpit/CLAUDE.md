# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

## Product Boundary

Cockpit owns the native command surface, client projection model, interaction,
and presentation. Phux owns durable work identity, execution authority, applied
Run policy state, ordered evidence, artifacts, and shared attention state.
Cockpit may author product and formation policy, but Phux records and enforces
the selected policy. Durable local work uses local Phux; Cockpit's direct local
PTY path is explicitly ephemeral and never grows a second coordinator. Read
`docs/PRODUCT_DIRECTION.md` and `docs/DURABLE_WORK_ARCHITECTURE.md` before
changing identity or orchestration.

The shipped app has one ahead-of-time TypeScript coordinator in `src/core.ts`
and declarative chrome in `src/app.native`. Zig remains behind
`src/native_extension.zig` for terminal/provider correctness; terminal cells
paint through `cockpit/native/terminal_painter.zig`, and
`workspace_projection.zig` remains the single geometry authority. The
`ts-chrome-parity` extension test is the chrome layout/accessibility gate.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:6cd5cc61 -->
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
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->


## Build & Test

The gate. **Judge it by the exit code and nothing else** — a fully green run has
previously ended with a line reading `failed command: .../test --listen=-`.

```sh
zig build test > /tmp/t.log 2>&1; echo "exit=$?"
```

Then read the verdict block, which the run prints last. Two fields in it are
load-bearing:

- **`source root:`** must name the worktree you edited. On 2026-08-12 a
  subagent's build silently ran in a sibling worktree; the exit code was real
  and described someone else's code, and nothing else in the output said so.
  Confirm `pwd` and `git rev-parse --show-toplevel` before treating any exit
  code as evidence.
- **`PASS, INCOMPLETE`** means `src/providers/phux/` was not in the build at
  all, because the phux client FFI has not been built in this checkout. The rest of the run is real;
  a change under that directory is not verified by it.

No verdict block at all means the run was not green, whatever else printed.

`-Dphux-enabled=true` swaps the *app* graph onto the phux provider. It is **not**
what compiles `src/providers/phux/` — FFI discovery is, and the flag alone never
was sufficient. Zig analyzes only what is referenced and nothing in Cockpit calls
`PhuxProvider.search`, so a 2026-08-10 signature drift passed at exit 0 under
every invocation including the full phux one. `src/providers/phux/ref.zig` plus a
`refAllDeclsRecursive` test in each rooted module is what forces the analysis.

README.md, "Reading the result of `zig build test`", has the FFI search order.
From the repository root, `just cockpit-test` builds the same-checkout FFI and
runs the shipping TypeScript graph plus retained native engine regressions.

### Running it

```sh
./scripts/dev-run.sh            # ReleaseSafe, foreground; ctrl-c ends it
./scripts/dev-run.sh --debug --detach
./scripts/dev-run.sh --automation   # -Dautomation=true, prints how to drive it
./scripts/dev-run.sh --phux         # the phux provider graph
```

It builds THIS checkout and runs it as a real app that cannot be mistaken for
`/Applications/Phux Cockpit.app`: its own bundle id (`dev.phux.cockpit.dev`),
its own process name (`phux-cockpit-dev`, so `pgrep -x` and System Events
target it rather than the installed copy), and its own config, workspace layout
and automation dropbox under `.dev-run/`. `.dev-run/app.log` is where the
runtime's own diagnostics land — read it after any chrome change, because
`zero_canvas_layout` and `zero_canvas_ui` are how a spacing or coverage mistake
announces itself. `./scripts/dev-isolation-check.sh` drives both apps at once
and proves the separation.

Whatever you run, check what binary a bug report is actually about. On
2026-08-12 `/Applications` held 0.7.1 built three days earlier while `main` was
0.8.0, so nothing merged in those three days had ever been in front of the
person filing render bugs.

### The other entry points

| Script | Answers |
|---|---|
| `scripts/automate-smoke.sh` | does the real bundle come up, present through the packet path, and respond to driven input? |
| `scripts/drive-shell-ceiling.sh --want N` | how many concurrent shells does the shipped bundle actually reach before one is refused? |
| `scripts/host-raster-check.sh` | does the host's real CoreText rasterizer still ink glyphs as thickly as the pinned baseline? |
| `scripts/host-raster-compare.sh <ref>` | did an SDK change move glyph pixels between two commits that both shipped? |
| `scripts/check-sdk-pin.sh` | does README name the sha `build.zig.zon` actually resolves? |
| `scripts/build-automation-cli.sh` | builds the only `native` CLI that can drive this app; the npm one is fingerprint-refused and always will be. |

## Architecture Overview

An ahead-of-time TypeScript + `.native` macOS app on the Native SDK fork
[`phall1/native`](https://github.com/phall1/native), pinned in `build.zig.zon`
by tarball sha. Cockpit is that fork's only real consumer, which is why breakage
arrives all at once at a pin bump — docs/SDK_PIN.md has the pre-bump checklist.

- `src/core.ts`, `src/app.native`, `src/windows/` — the one app coordinator and
  declarative chrome.
- `src/cockpit/` — native engine, topology, layout, session state, providers,
  and terminal runtime. `src/cockpit/native/` is the projection/paint boundary.
- `src/providers/` — `contract.zig` holds provider-neutral identity; `local/`
  runs PTYs in-process, `phux/` reaches a remote phux across an FFI ABI.
- `src/terminal/` — one `libghostty-vt` session per terminal, projected into a
  canvas grid. The emulator owns cell state, damage, scrollback and selection.
- `src/config/`, `src/tests/`.

Three invariants that shape most changes:

- **Identity is provider-backed.** Tab, pane, window, PID and effect-key
  position are never durable identity.
- **One geometry.** `layout.resolve()` is the single source of pane rects for
  the painter, the hit-test widget tree and the PTY sizing pump. They were three
  independent derivations once, and had already drifted.
- **Two rasterizers.** On glass the AppKit host decodes the packet path and
  draws with CoreText. `native automate screenshot` runs a pure-Zig CPU
  reference renderer that never calls CoreText. Only one of them is on screen.

## Conventions & Patterns

**An assertion you have not seen fail is not evidence.** Assert absent, act,
assert present. A merged and publicly retracted "automation input is broken"
finding came from an assertion that could not move: it counted
`role=textbox name="Terminal`, which only ever matches the SELECTED tab, so a
new tab could not shift it however perfectly the key was delivered. The
`expect_change` helper in `scripts/automate-smoke.sh` encodes the shape. Give
any assertion you add its before-half too.

**Never compare against a counterfactual you authored.** The font-smoothing fix
was validated by hand-editing the SDK into a state no build was ever in; against
the real prior commit it moves zero pixels. Compare two things that shipped —
`scripts/host-raster-compare.sh` exists for exactly this.

**A screenshot cannot answer a glyph question.** The automation screenshot path
is structurally blind to everything CoreText does: outlines, hinting, smoothing,
CG blend arithmetic, device colour space. Never conclude "the text renders
correctly" from one. Read docs/RENDER_FIDELITY.md before writing any test that
claims to check what the terminal looks like.

**Live-app automation is serial-only** (`phux-cockpit-2ml.10`). The automation
dropbox is per-user rather than per-process and activation targets a process by
NAME, so a second bundle silently steals the channel, and a snapshot of a dead
instance returns an empty widget tree instead of an error. Assert
`publisher_pid` is the pid you launched before believing anything. Guard with
`pgrep -x <exename>` or `pgrep -f "[p]attern"`; plain `pgrep -f` matches the
shell running it, which is fatal inside a wait loop and quietly wrong outside
one.

**Measure constants, never pick them**, and keep the deriving command beside the
number — `--min-solid 4000` and the 32-shell pty ceiling both carry theirs.

**Show every regression test failing first.** Disable the fix, watch red,
restore, watch green, and report what you disabled.

Settled questions live in docs/DECISIONS.md; reopen one by adding information,
not by re-litigating it. Track work with `bd` and keep durable knowledge in
`bd remember`. No emoji in committed files. This codebase writes substantial WHY
comments rather than restating code — match that density.

**AGENTS.md and this file are independent**, not symlinked, and they have
already drifted: AGENTS.md carried the product contract, the rendering-evidence
warning and the test gate while this file was still scaffold placeholders.
AGENTS.md remains the product contract and quality bar; this file is the build,
the map and the working rules. Mirror substantive changes into both.
