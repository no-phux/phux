---
audience: contributors
stability: stable
last-reviewed: 2026-10-14
---

# 0167 — Project identity is a session attribute keyed by remote

**TL;DR.** A session created in a git checkout carries a typed project
identity among its attributes: the normalized `origin` URL (else the
host-scoped canonical common git dir), its checkout, the branch at
creation, and whether it is a linked worktree. The client reads git files
and sets it with `SESSION_CREATE`; the snapshot reports it. Pickers, the
sidebar, and `phux ls` group by it, so one repository groups across machines.

Status: Accepted
Date: 2026-10-14

## Context

`phux worktree` ([ADR-0054](./0054-worktree-bound-sessions.md)) and
`phux project` ([ADR-0152](./0152-project-recipes-run-only-approved-bytes.md))
create sessions in checkouts, but nothing recorded which repository a
session belongs to. The only project fact was `phux.session.project/v1`, a
free-text label holding one `name\0project` pair for the whole server, so
at most one session was ever tagged. Session records already carry
descriptive attributes, set by `SESSION_CREATE`'s `attr` field
([ADR-0161](./0161-organization-is-typed-commands-behind-a-feature-bit.md)),
but the server kept only `project`, `directory`, and `host`, and the
snapshot every client reads did not report them.

herdr groups workspaces by the canonical git common directory read from
the filesystem: the primary checkout is the parent, linked worktrees are
children, each group collapses. That key is a local path, so the same
repository on two machines is two groups.

Project, directory, repository, and host stay attributes of a session, not
a container above it (#1108, #1152, #1159). The server does not run git.

## Decision

1. **Identity.** `project.id` is the `origin` remote URL normalized:
   scheme, userinfo, default port, trailing `/` and `.git` removed, host
   lowercased, scp form (`git@host:a/b`) read as `host/a/b`. So
   `git@github.com:a/b.git` and `https://github.com/a/b` are both
   `github.com/a/b`. Without a network `origin` it is
   `path:<host>:<canonical common dir>`. Beside it: `project.name` (a
   `[[projects]]` label, else the repository name), `project.checkout`,
   `project.branch` at creation (descriptive, never a key),
   `project.linked_worktree` (git dir differs from common dir), and
   `project.host` (where git was read).
2. **Computed by the client from files.** Discovery walks up to `.git`,
   follows a `gitdir:` file and `commondir`, reads `HEAD` and the common
   `config`. No `git` process. A bare repository is its own common dir.
   `insteadOf` rewrites and includes are not applied.
3. **Set at creation, typed on the record.** `phux new`, `phux worktree
   new|open`, and `phux project open` pass the identity as `attr` pairs in
   the `SESSION_CREATE` that creates the session, when the server is local
   and the directory is in git. The server parses the `project.*` pairs
   into one typed, serde-defaulted field of the session's attributes and
   persists it with the record; a value over 256 bytes is not sent. One
   write, at creation: no read-modify-write and no second store.
4. **Reported where clients already look.** The snapshot's per-session
   `SESSION_STATE` gains a repeated `attr` field carrying the session's
   attributes, and organization records carry them as before. The TUI,
   `phux ls --json`, and the `phux.hosts/v1` rows
   ([ADR-0140](./0140-sidebar-machines-come-from-a-hosts-provider.md))
   read the identity from there; `ls` JSON gains an additive `project`
   object.
5. **Grouping is client-side and membership-only.** Sessions with equal
   `project.id` form a group; a legacy label still groups. Primary
   checkouts lead, each followed by linked worktrees from its host,
   indented. Groups keep the first-appearance order of their input, so the
   sidebar keeps [ADR-0112](./0112-stable-split-sidebar-navigation.md)'s
   stable order and the picker its needs-you order. A header rolls up how
   many panes in it need a person. In the sidebar a group of two or more
   collapses on click; the collapsed set is per-client state on disk, and
   a collapsed group still lists its first session, the current one, and
   any session that needs you.
6. **Across machines.** Each machine's `ls --all` row carries its sessions'
   identities, so another machine's session in the same repository joins
   the group.

## Why

A remote URL is the one identity two clones share; a path is not. Making
it the key lets "the phux repo" mean the same thing on a laptop and a
build box. Reading files keeps discovery cheap and testable and keeps git
off the server. Typed attributes on the session record put the identity
where the session already lives: it is created, persisted, renamed, and
deleted with the session, and it reaches every client through the snapshot
it already reads.

## Tradeoffs

- Only sessions created through those verbs carry an identity. A session
  created in the TUI, or a generated-name `phux new` that creates at
  attach, has none until a command can set attributes on an existing
  session.
- Forks with different `origin` URLs are different projects; a repository
  whose `origin` changes after creation keeps its old identity.
- Federation satellites ([ADR-0107](./0107-satellite-sessions-are-listed-never-adopted.md))
  report sessions without attributes; they stay under their host.
- A server without `ORGANIZATION` creates through the legacy path and
  records no identity.

## Alternatives

**A shared metadata key holding every session's identity.** One
uninterpreted document invites a read-modify-write race between creators
and a second copy to reconcile with the session list. Rejected.

**A Project record above Session.** Rejected again: it reintroduces the
container #1108 removed.

**Run `git` on the client.** Correct but slower and harder to test; the
files are a stable format.
