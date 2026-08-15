---
audience: agents, contributors
stability: stable
last-reviewed: 2026-05-28
---

# phux Project Instructions for Agents

**TL;DR.** phux-specific agent guidance layered on [`AGENTS.md`](./AGENTS.md)
(universal rules): how to build and test (`nix develop`, `just ci`), the
crate/architecture map, and the project conventions to follow when changing
code or docs.

See [`AGENTS.md`](./AGENTS.md) for universal agent instructions
(shell hygiene, session completion protocol). This file adds
phux-project-specific guidance: build, test, architecture, and conventions.

---

## Build & Test

The dev shell is Nix-pinned (`flake.nix`): Rust 1.90, `zig_0_16` for
libghostty-vt's build, plus `nextest`, `deny`, `watch`, `insta`,
`mutants`, `just`.

```bash
nix develop      # or direnv allow once
just ci          # the inner loop: every deterministic gate CI runs
just ci-full     # ci + the real-server e2e and agent smoke lanes
just check       # quick type-check
just test        # cargo nextest run --workspace
```

**`just ci-full` is the complete PR bar; `just ci` is the inner loop.** CI's
`test` job runs `cargo nextest run --workspace` *plus* `just e2e` *plus*
`just agents-fleet-smoke`, and only the first of those is inside `just ci` —
so a branch can be `just ci` green and still fail a required check. `just e2e`
is kept out of `just ci` deliberately (it spawns real PTY-backed servers, so a
`just ci` failure always means a real defect); `just ci-full` is where you pay
for that coverage. See CONTRIBUTING.md §"Bar for any change" for the
gate-by-gate map.

Note `commitlint` is a required check and lints **every commit in the PR**,
not just the title — one malformed message fails the branch.

## How work reaches `main`

**A maintainer may push straight to `main`.** `main` carries two rulesets:

| Ruleset | Rules | Bypass |
|---|---|---|
| `main safety rails` | `deletion`, `non_fast_forward` | **none** |
| `main merge gate` | required checks `check` / `test` / `commitlint` | admin, always |

So a direct push is allowed, and `main` still cannot be force-pushed or
deleted **by anyone, including an admin** — those two live in a separate
ruleset with an empty bypass list precisely so that loosening the merge gate
never loosens the safety rails.

The gate that replaces the PR is therefore **local**: run `just ci-full`
before pushing. Nothing else will catch you. A PR is still the right shape for
anything you want reviewed, for anything from outside the maintainer set, and
for release-please's own branch — pushing straight to `main` is a convenience
for work you have already validated, not a licence to skip validating it.

The reason this is set up this way rather than routing everything through PR
CI: a PR run costs several minutes of Actions time per branch, and for a
maintainer who has already run the identical gates locally that is pure
duplication. The release lanes still run the full suite, so the artifact that
reaches users is never gated on a local run alone.

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

Crates: sixteen, all under `crates/*`, all workspace members. The ones
you touch most: `phux-protocol` (wire), `phux-core` (domain),
`phux-server` (daemon), `phux-client` (renderer + ratatui chrome),
`phux-client-core` (pane-interior substrate and session kernel; no
`ratatui` dependency, so the boundary is compiler-enforced),
`phux-client-ffi` (stable native C ABI over that kernel, for non-Rust
embedders), `phux-config` (TOML + widgets), `phux` (binary). The other
eight are narrow single-purpose surfaces. Every crate has a section in
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
  `phux-protocol`; `phux-server` and `phux-client` consume them.
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
