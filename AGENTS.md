---
audience: agents, contributors
stability: stable
last-reviewed: 2026-09-27
---
# Agent Instructions

**TL;DR.** `mise install` or `nix develop`, then `just doctor` and a scoped
check. Work in an isolated branch/worktree, use non-interactive shell commands,
track agent work with Beads, commit verified changes before handoff, and report
actual validation. Project architecture lives in CLAUDE.md; setup is one guide.
<!-- bd-doctor-divergence: ok -->

## Setup and validation scope

- Start at [`docs/SETUP.md`](./docs/SETUP.md): `mise install` or
  `nix develop`, then `just doctor <area>` and the smallest relevant gate.
  Expand validation for shared APIs, protocol/FFI, Cargo inputs, or build
  scripts. Report exact checks; a scoped pass is not a full CI pass.
- Beads is maintainer/agent task tracking, not a contributor gate; outside
  contributors can use a GitHub issue/PR.
- Use non-interactive shell commands (`-f` / `-y` where appropriate).
- [`CLAUDE.md`](./CLAUDE.md) holds architecture and code conventions;
  [`docs/CONVENTIONS.md`](./docs/CONVENTIONS.md) governs documentation.

## Worktree Isolation

- **Never implement on `main` or in the primary repository worktree.** Create
  a dedicated branch and linked worktree from current `main` for all code,
  docs, tests, and commits. The primary worktree is for integration only:
  update `main`, fast-forward verified work, push, remove finished worktrees.
- Never stash, reset, clean, or overwrite a dirty primary worktree; isolate
  your work in a new worktree instead.

## Never touch the installed phux

- The user's installed `phux` and its server are production. Never copy a
  build over an installed binary (`~/.local/bin/phux`, Homebrew, and so on),
  never aim a build at the production socket (`--socket`, `PHUX_SOCKET`,
  `PHUX_PROFILE=default`) or its state (an inherited `PHUX_WS_TOKENS` /
  `PHUX_WS_TLS_*` from a phux pane), and never `phux upgrade` the
  production server.
- Verify fixes against the dev-profile server (`just rebuild`), or an
  explicit temp socket. Shipping a fix to the user's machine means a release
  they install, not a hand-deployed build. See
  [`docs/operations.md`](./docs/operations.md) "Hard guards".

## Finish the work

- **Commit verified task changes before the final handoff.** This is standing
  authorization for local commits on the isolated feature branch; do not ask
  whether to commit. An explicit "do not commit" wins.
- Stage only task-owned changes, make scoped conventional commits, and report
  commit IDs and checks run.
- When asked to push, merge, or land on `main`, carry it through integration
  and validation without re-asking, and keep local `main` synchronized.
  Ownership boundaries, branch protections, and no-push instructions still
  apply; local commit authorization does not authorize remote operations.
- Routine commits, pushes, merges, and cleanup are steps of the existing
  task, not new Beads tasks.
- If a step is genuinely blocked, report what failed and the exact remedy.

This policy overrides the conservative/minimal commit defaults in the managed
Beads guidance below and in `bd prime`.

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
