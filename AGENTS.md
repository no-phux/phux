---
audience: agents, contributors
stability: stable
last-reviewed: 2026-09-05
---
# Agent Instructions

**TL;DR.** Select setup and validation by the area you change. Work in an
isolated branch/worktree, use non-interactive shell commands, track agent work
with Beads, and report actual validation. Project architecture and coding
conventions live in CLAUDE.md; contributor setup has one canonical guide.
<!-- bd-doctor-divergence: ok -->

## Setup and validation scope

- Start at [`docs/SETUP.md`](./docs/SETUP.md). Native tools and Nix are supported;
  select prerequisites by the work area instead of installing the full shell.
- Run `bash scripts/doctor.sh <area>` for prerequisites and the smallest relevant
  gate first. Expand validation for shared APIs, protocol/FFI, Cargo inputs, or
  build scripts. Report exact checks; a scoped pass is not a full CI pass.
- Keep setup/version details in that guide and the toolchain pins, not in agent
  instruction files. Browser engine regeneration uses verified pinned source.
- Beads is maintainer/agent task tracking, not a compiler or contributor gate
  dependency. Outside contributors can use a GitHub issue/PR without installing
  the maintainer orchestration stack.
- Use non-interactive shell commands (`-f` / `-y` where appropriate).
- See [`CLAUDE.md`](./CLAUDE.md) for architecture and code conventions;
  [`docs/CONVENTIONS.md`](./docs/CONVENTIONS.md) governs documentation.

## Worktree Isolation

- **Never implement on `main` or in the primary repository worktree.** Before
  editing, create a dedicated branch and linked worktree from current `main`;
  make all code, documentation, test, and commit changes there.
- Reserve the primary worktree for integration only: update `main`, squash or
  fast-forward verified feature work, push, and remove completed worktrees.
- If the primary worktree is dirty or another agent is using it, do not stash,
  reset, clean, or overwrite those changes. Leave them untouched and isolate
  your work in a new worktree.

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
