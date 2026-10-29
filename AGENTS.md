# Agent instructions

Rules for coding agents working in this repository. Humans: see
[CONTRIBUTING](./CONTRIBUTING.md). Architecture and conventions are in
[docs/architecture.md](./docs/architecture.md); setup and gates in
[docs/development.md](./docs/development.md).
Live contracts and their rationale belong in the current docs. For an old
`ADR-NNNN` citation, use the [searchable historical snapshot](./docs/architecture.md#historical-decisions).
Do not treat an obsolete proposal as shipping behavior; if an operative
constraint is missing from the live docs, verify it against code/tests and
restore that constraint rather than silently dropping it.

<!-- bd-doctor-divergence: ok -->

## Using phux as an agent

The rest of this file is how to change the phux repository. To install
phux and drive sessions from code, follow
[Using phux as an agent](./docs/agent-quickstart.md).

```sh
curl -fsSL https://phux.sh/install | sh
phux server --ensure
phux new --json -s agent -c "$PWD" -- claude
```

`phux` and `phux attach` need a terminal. Scripts use `--json` and address
panes as `@N` from `terminal_id`. `phux skill` prints the guide built into
the binary you have installed.

## Build and validate

`mise install` or `nix develop`, then `just doctor <area>` and the smallest
gate that covers the change (`just crate-check <crate>`, `just test`,
`just check` for the diff-routed PR bar). Widen for shared APIs,
protocol, FFI, Cargo inputs, or build scripts. Report the exact checks you
ran; a scoped pass is not a full pass. Use non-interactive flags (`-f`,
`-y`, `--yes`).

A CLI, config, or widget change needs `just docs-gen`; a wire change needs
the schema regenerated ([protocol](./docs/protocol.md#changing-the-wire)).
`just docs-check` gates the hand-written docs.

## Worktree isolation

Never implement on `main` or in the primary checkout. Make a branch and a
linked worktree from current `main` for every change. The primary checkout
is for integration only: update `main`, fast-forward verified work, push,
remove finished worktrees. Never stash, reset, clean, or overwrite a dirty
primary worktree.

One agent session per worktree. Build and test recipes lease the worktree
to the session that runs them (`PHUX_WORKTREE_OWNER`, else the harness
session id) and refuse another live session with exit 3; `test` and
`e2e` also fail if HEAD moves or the lease changes hands mid-run. Hand a
worktree on with `just worktree-release`. An idle lease is taken over after
six hours; `PHUX_WORKTREE_TAKEOVER=1` is for a deliberate handoff only. The
guard does not stop raw `git` or editor use, so isolation is still yours to
keep (`scripts/lib/worktree-owner.sh`).

## Never touch the installed phux

The user's installed `phux` and its server are production. Never copy a
build over an installed binary (`~/.local/bin/phux`, Homebrew, and so on),
never aim a build at the production socket (`--socket`, `PHUX_SOCKET`,
`PHUX_PROFILE=default`, or `PHUX_WS_TOKENS` / `PHUX_WS_TLS_*` inherited from
a phux pane), and never `phux upgrade` the production server. Verify
against the dev-profile server (`just rebuild`) or an explicit temp socket.
A fix reaches the user's machine as a release they install.

## Finish the work

Commit verified task changes on the feature branch before handing off; this
is standing authorization for local commits, and an explicit "do not
commit" wins. Stage only task-owned changes, write scoped conventional
commits, and report commit ids and the checks run. When asked to push,
merge, or land on `main`, carry it through integration and validation
without re-asking and keep local `main` synchronized; ownership boundaries,
branch protection, and no-push instructions still apply. Routine commits,
pushes, merges, and cleanup are steps of the task, not new Beads issues. If
a step is blocked, say what failed and the exact remedy.

This policy overrides the conservative commit defaults in the managed Beads
block below and in `bd prime`. Beads is maintainer and agent task tracking,
not a gate for outside contributors.

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
