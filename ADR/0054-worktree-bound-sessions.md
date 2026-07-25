---
audience: contributors
stability: stable
last-reviewed: 2026-07-25
---

# 0054 — Worktree-bound sessions by name convention

**TL;DR.** `phux worktree` composes `git worktree` with the existing session
verbs. The binding between a checkout and a phux session is a pure function of
the worktree path — a derived session name — not stored state. The server
learns nothing about git, and no wire message changes.

Status: Accepted
Date: 2026-07-25

## Context

Running several coding agents at once means running several checkouts at once.
Today that is two manual steps every time: `git worktree add`, then `phux new
-s something -c that-path`, with the operator remembering which session belongs
to which checkout. Removing a checkout is worse — the session outlives the
directory, and its panes sit in a path git has deleted underneath them.

Every multiplexer that has solved this has done it by teaching its daemon about
git. That is the option to reject deliberately, not by omission.

## Decision

`phux worktree` is a client-side convenience layer over `git worktree` plus the
already-shipped `new` / `ls` / `kill` verbs. It adds four actions — `list`,
`new`, `open`, `remove` — and no server state, no protocol field, and no
persistent mapping file.

A worktree binds to exactly one session name, derived from the worktree's
directory basename by a total, deterministic function: characters outside
`[A-Za-z0-9._-]` collapse to `-`, runs of `-` collapse to one, leading
selector-reserved characters (`@`, `#`, `=`, `.`) are stripped, and the empty
or `.`-only result falls back to `worktree`. The derivation is pure, so any
client — the CLI, the TUI, an agent — computes the same name from the same path
without consulting anything.

`list` reports each worktree with its derived name and whether a session by
that name currently exists. `new` runs `git worktree add` and then creates the
session rooted in the new checkout. `open` creates the session if absent and is
otherwise a no-op that prints the name. `remove` kills the bound session first,
then runs `git worktree remove`, and refuses a dirty checkout unless `--force`
is passed.

## Rationale

The derived name is correct at every instant because it is never stored. A
mapping table would need invalidation on every event phux cannot observe: git
deleting a worktree, an operator `mv`-ing the directory, a session renamed
through `phux rename`. Deriving on demand has no stale state to reconcile
because it has no state.

It also keeps the layering honest. A worktree is a VCS concept; a Terminal is
the phux unit (ADR-0009). Teaching `phux-server` about git would put a
filesystem-watching, git-parsing subsystem inside the one process that must
stay alive across every client crash. The composition lives where composition
belongs — in the CLI, next to the other verbs it composes.

Killing the session before removing the directory is ordered that way because
the reverse order is the failure users actually hit: git refuses to remove a
worktree whose files are held open, and a shell with that cwd holds it open.

## Tradeoffs

Two worktrees whose basenames collide derive the same session name. `new`
detects the collision and fails rather than adopting the other checkout's
session; the operator passes `-s NAME` to disambiguate. This is accepted
because the alternative — auto-suffixing — produces names nobody can predict,
which defeats the point of a derivable binding.

A session created by hand in a worktree path, under a different name, is not
recognized as bound. `list` will show the worktree as unbound even though a
session is sitting in it. Fixing this needs pane cwd in the session snapshot,
which is a protocol addition this ADR deliberately does not make.

`remove` shells out to git for the dirty check rather than reimplementing it,
so its refusal semantics track whatever the installed git does.

## Alternatives considered

**Server-side worktree registry.** A `WORKTREE_*` message family with the
daemon owning the mapping. Rejected: it makes the server depend on git being
installed and on a directory the server does not control, for a feature that
composes fine without it.

**Sidecar mapping file** (`$XDG_STATE_HOME/phux/worktrees.json`). Rejected for
the staleness argument above — every path that mutates a worktree outside phux
silently corrupts it.

**Session cwd in the snapshot, matched against `git worktree list`.** This is
the correct long-term answer and is strictly better than name derivation, but
it is a wire change with its own compatibility surface. Filed rather than
bundled; the derived name is forward-compatible with it because a cwd-matched
binding can only ever be a superset of the name-matched one.
