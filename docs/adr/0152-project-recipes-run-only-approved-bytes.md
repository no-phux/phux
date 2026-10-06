---
audience: contributors, agents
stability: stable
last-reviewed: 2026-10-06
---

# 0152 — Project recipes run only approved bytes

**TL;DR.** `phux project open` attaches to a checkout's session and, when that
session is not running, builds it from the checkout's `.phux/project.toml`:
windows, panes, splits, commands, environment, and focus. The recipe is
compiled into the `workspace restore` archive and replayed by that engine, so
there is no second session builder. A repository recipe runs only after this
machine approved its exact bytes for that repository; `[[projects]]` in
`config.toml` names checkouts and may point at an out-of-repo recipe, which is
trusted because the user wrote it there.

Status: Accepted
Date: 2026-10-06

## Context

phux could restore a saved workspace
([ADR-0129](./0129-projections-are-named-by-key.md)) and bind a
session to a worktree ([ADR-0054](./0054-worktree-bound-sessions.md)), but a
repository could not describe the session it wants: an editor beside an
agent, a dev server, a test watcher. Every user rebuilt it by hand or
maintained shell scripts of `phux spawn` calls. Comparable multiplexers (tmux
session files, fut's `.fut/project.toml`) solve this with a declarative recipe
checked into the repository.

A repository-owned file that starts processes is code from whoever wrote the
repository. Opening a freshly cloned project must not run it silently.

## Decision

1. **Recipe.** `.phux/project.toml` at the checkout root (the git work tree,
   or the directory outside git) declares `env`, `focus = "WINDOW.PANE"`, and
   `[[windows]]` with `id`, `name`, `cwd`, `env`, and `panes`. Each pane has
   optional `id`, `command` (an argument vector, never shell-evaluated),
   `exec`, `cwd`, `env`, and, after a window's first pane, `split = { target,
   direction = "right" | "down", ratio }` naming an earlier pane. Unknown keys,
   bad ids, dangling targets, missing directories, `PHUX_*` variables, and
   more than 64 panes are refused before anything starts.
2. **One engine.** The recipe compiles to a one-session workspace archive; the
   archive pane gains an optional `env` map. `workspace restore`'s replay
   creates the panes, writes the split tree, and rolls the whole session back
   if any pane fails.
3. **Commands fall back to a shell.** Without `exec = true`, a pane runs
   `/bin/sh -c '"$@"; exec "${SHELL:-/bin/sh}"' phux-project ARGV...`, so a
   stopped dev server leaves a prompt in its pane. The wrapper is fixed text;
   the recipe's arguments are positional parameters, never interpolated.
4. **Creation only.** The session name is ADR-0054's pure function of the
   checkout path. When that session is live, `open` attaches (or, with
   `--json`, reports its seed pane) and never rereads or reconciles the
   recipe.
5. **Approval is exact bytes per repository.** An approval records the
   repository identity (the canonical git common directory, so every worktree
   of one repository shares it, or the directory outside git), the recipe's
   path relative to the checkout, and the SHA-256 of its bytes, in the
   owner-only `<state dir>/trusted-projects.toml`, rewritten atomically under
   a lock. One changed byte, or the same bytes in another repository, is
   untrusted. An unreadable or malformed store trusts nothing.
6. **Approval is explicit.** `phux project trust` validates and approves;
   `untrust` withdraws; `status` exits 0 trusted, 1 untrusted or absent, 2
   undeterminable. An interactive `open` shows the full recipe and asks; a
   non-interactive or `--json` open refuses with exit 2. `project init`
   writes phux's own starter recipe and approves exactly those bytes.
7. **Catalog.** `[[projects]] name, path, recipe?` lets `phux project open
   NAME` work from anywhere. phux never scans for repositories. A `recipe`
   path in `config.toml` is trusted without approval and cannot be untrusted
   except by removing it.

## Why

Reusing the archive keeps one tested path for building sessions, including
rollback, and makes a recipe and a saved workspace the same thing at
different stages. Keying approval on the repository rather than the file
path lets a fleet of worktrees open from one decision, while keying on the
repository rather than the bytes alone stops a hostile clone from borrowing
an approval for a recipe that runs repository-relative programs.

## Tradeoffs

- A recipe cannot express agent-session resume or retained exits; it builds
  fresh panes.
- Editing a recipe means re-approving it, even for whitespace.
- The wrapper's fallback uses the pane's `$SHELL`, not `defaults.shell`.

## Alternatives

**Trust on first open without showing the file.** Rejected: it is the silent
execution the approval exists to prevent.

**A server-side project resource.** Rejected: the server knows no git
(ADR-0054), and a recipe is a client-side creation plan, not live state.

**Reconcile live sessions against the recipe.** Rejected: it would kill or
spawn processes in a session the user has since rearranged.
