---
audience: agents, contributors
stability: stable
last-reviewed: 2026-09-08
---

# phux Project Instructions for Agents

**TL;DR.** phux-specific agent guidance layered on [`AGENTS.md`](./AGENTS.md)
(universal rules): how to build and test (`nix develop`, `just ci`), the
crate/architecture map, and the project conventions to follow when changing
code or docs.

See [`AGENTS.md`](./AGENTS.md) for universal agent instructions
(shell hygiene, session completion protocol). This file adds
phux-project-specific guidance: build, test, architecture, and conventions.

**Commit verified task changes before handoff** unless the user explicitly
requests otherwise. Follow [AGENTS.md's completion policy](./AGENTS.md#finish-the-work):
local commits are authorized by default, an explicit request to land work is
carried through integration, and routine commit/push/merge/cleanup steps do not
get separate Beads tasks. That repository policy overrides the conservative
commit defaults in the managed Beads block and `bd prime`.

---

## Build & Test

Start at [`docs/SETUP.md`](./docs/SETUP.md). Native tools and Nix are supported;
select prerequisites by the work area instead of installing the full shell.
Run `bash scripts/doctor.sh <area>` for prerequisites and the smallest relevant
gate first. Expand validation for shared APIs, protocol/FFI, Cargo inputs, or
build scripts. Report exact checks; a scoped pass is not a full CI pass.
Keep setup/version details in that guide and the toolchain pins, not in agent
instruction files. Browser engine regeneration uses verified pinned source.
Beads is maintainer/agent task tracking, not a compiler or contributor gate
dependency; outside contributors can use a GitHub issue/PR without it.

```bash
just doctor native  # or core / docs / integrations / web / cockpit / ci
just core-check     # example scoped loop; see SETUP.md for other areas
just ci             # full root deterministic/unit gate set
just ci-full        # ci + the real-server e2e and agent smoke lanes
just check       # quick type-check
just test        # cargo nextest run --workspace
```

**`just ci-full` is the full root PR bar; scoped gates are the inner loop.** CI's
`test` job runs `cargo nextest run --workspace` *plus* `just e2e` *plus*
`just agents-fleet-smoke`, and only the first of those is inside `just ci` —
so a branch can be `just ci` green and still fail a required check. `just e2e`
is kept out of `just ci` deliberately (it spawns real PTY-backed servers, so a
`just ci` failure always means a real defect); `just ci-full` is where you pay
for that coverage. See CONTRIBUTING.md §"Bar for any change" for the
gate-by-gate map.

`commitlint` lints **every commit in the PR**, not just the title. The live
ruleset verified on 2026-09-09 requires `ci` and `commitlint`.

## How work reaches `main`

`main` currently carries one active ruleset:

| Ruleset | Rules | Bypass |
|---|---|---|
| `main` | deletion, non-fast-forward, linear history, pull request, required `ci` / `commitlint` | organization admin and the release App, always |

Ordinary maintainers cannot push directly. An organization administrator can
use the bypass, including for the one-time Cockpit history import documented in
`docs/RELEASING.md`; use a non-force fast-forward even when bypass is available.
The aggregate `ci` check covers compile-free guards and the routed product
lanes; raw `check` and `test` jobs remain visible.

An administrator bypass never replaces validation: run `just ci-full` and use a
PR for review and hosted checks before any exceptional fast-forward. Normal
changes and Release Please branches use the ordinary PR path.

Cockpit-only diffs are routed away from the Rust compile lanes while retaining
the required root contexts; shared FFI/protocol/Cargo inputs run both surfaces.
See `docs/RELEASING.md` for the routing matrix.

## Architecture Overview

phux is a **libghostty-backed terminal control plane**. The wire is
asymmetric: server→client *terminal content* is **VT bytes** forwarded
from the PTY ([ADR-0013](./ADR/0013-libghostty-bytes-on-wire.md));
client→server *input* is **structured key, mouse, focus, and paste
events** built from libghostty's atoms (ADR-0006, ADR-0008). The
protocol is layered as L1 Terminal substrate + L2 Collection + L3
Metadata ([ADR-0015](./ADR/0015-protocol-layering.md)). One server per
user ([ADR-0003](./ADR/0003-server-process-model.md)); one tokio
current-thread runtime; UDS transport with a QUIC future
([ADR-0007](./ADR/0007-mosh-class-transport-and-satellites.md)).

Authoritative docs, in order of priority:

- [`docs/CONCEPTS.md`](./docs/CONCEPTS.md) — canonical mental model.
  Read this first if you haven't.
- [`docs/CONVENTIONS.md`](./docs/CONVENTIONS.md) — doc system,
  frontmatter, TL;DR rule, ADR template.
- [`docs/spec/`](./docs/spec/) — normative wire protocol. Code conforms
  to it, not vice versa.
- [`docs/architecture/`](./docs/architecture/) — internal structure
  (process model, crate graph, data model, threading, transport).
- [`docs/consumers/tui.md`](./docs/consumers/tui.md) — TUI consumer
  surface (CLI, config, keybindings, status bar, hooks).
- [`docs/operations.md`](./docs/operations.md) — error model, logging,
  security boundaries.
- [`docs/vision.md`](./docs/vision.md) — the long arc.
- [`ADR/`](./ADR/) — decisions, with rationale and tradeoffs.

Crates: eighteen, all under `crates/*`, all workspace members. The ones
you touch most: `phux-protocol` (wire), `phux-core` (domain),
`phux-server` (daemon), `phux-tui` (the attach driver, libghostty
replicas, and ratatui chrome), `phux-client` (the headless client library
behind the agent verbs and MCP; no `ratatui`), `phux-client-core`
(pane-interior substrate and session kernel; no `ratatui` and no `tokio`,
so both boundaries are compiler-enforced, ADR-0020 and ADR-0100),
`phux-client-ffi` (stable native C ABI over that kernel, for non-Rust
embedders), `phux-config` (TOML + widgets + the settings catalogue), `phux` (binary).
The other nine are narrow single-purpose surfaces. Every crate has a section in
[`docs/architecture/module-structure.md`](./docs/architecture/module-structure.md)
— read it before assuming a capability is missing.
`phux-protocol` is publishable; the rest are `publish = false`.

## Conventions & Patterns

- **Doc system is layered.** Read [`docs/CONVENTIONS.md`](./docs/CONVENTIONS.md)
  before adding or moving docs. Every `.md` outside `README.md` carries
  YAML frontmatter (audience/stability/last-reviewed) and a `**TL;DR.**`
  block. `just docs-check` enforces the gates in CI.
- **No emojis in committed files.** Plain prose only.
- **Conventional commits.** `feat(scope): ...`, `fix(scope): ...`,
  `docs(scope): ...`, `chore(scope): ...`.
- **`docs/spec/` is normative.** Wire changes update the relevant
  `docs/spec/*.md` + add an entry to `docs/spec/CHANGELOG.md` + bump
  version (see CONTRIBUTING.md). Wire bytes are owned by
  `phux-protocol`; `phux-server`, `phux-client`, and `phux-tui` consume them.
- **ADR for any decision that closes off a design space.** Strict
  template per `docs/CONVENTIONS.md` — controlled `Status:` vocabulary,
  ~150-line cap. Bug fixes don't need an ADR; "should this be in `core`
  or `server`?" does.
- **`unsafe` requires a `// SAFETY:` comment.** Library crates default
  to `forbid(unsafe_code)`.
- **No new deps without a paragraph of justification in the PR.**
- **Linear history on `main`.** Rebase, ff-only merges; no `--no-ff`.
- **Never implement on `main` or in the primary repository worktree.** Create
  a dedicated branch and linked worktree from current `main` before editing;
  make code, docs, tests, and commits there. The primary worktree is for
  integration only: update `main`, squash or fast-forward verified work, push,
  and remove completed worktrees. Never stash, reset, clean, or overwrite a
  dirty primary worktree to make room.
- **Multi-agent fan-out uses self-managed worktrees** — see
  CONTRIBUTING.md §"Multi-agent fan-out" for the wave-1 race that
  motivated this.


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
