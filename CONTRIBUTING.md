# Contributing

phux is picky about what it takes on. The multiplexers before it each grew
a scripting language, a plugin host, a config DSL, and a copy mode, and that
accreted surface is now the thing nobody can finish refactoring. Every
feature declined is one that never has to keep working across every future
version of the wire. If a change adds surface, expect to argue for it; a
[proposal in an issue](https://github.com/no-phux/phux/issues) before a PR saves
everyone time.

## Setup

```sh
mise install        # or: nix develop
just doctor
just check
```

[Development](./docs/development.md) has the per-area setup, the gates, and
what CI runs. [Architecture](./docs/architecture.md) says where things are
and why. A GitHub issue or PR is enough to coordinate.

## Bar for a change

- The applicable CI lanes pass. Iterate with the scoped gates; run
  `just check` before pushing; it runs the lanes CI selects from the diff.
- Test what you change. Wire changes get `proptest` roundtrips and `insta`
  snapshots; state machines get transition tests; bug fixes get a
  regression test that fails on the bug.
- A CLI, config, or widget change regenerates `docs/reference/` with
  `just docs-gen`. A wire change regenerates `docs/spec/wire-schema.json`
  and is gated on a feature bit, not a version bump
  ([protocol](./docs/protocol.md#changing-the-wire)).
- No new dependency without a paragraph in the PR saying why.
- `unsafe` needs a `// SAFETY:` comment naming the invariant. Library crates
  forbid it by default.
- Public items in library crates have doc comments.
- No emoji in committed files.

## Docs

Hand-written pages live in `docs/*.md` and are short on purpose. Anything
the binary can state (commands, flags, settings, environment variables,
exit codes, hook events, wire frames) is generated into `docs/reference/`
and `docs/spec/`; link to it rather than restating it, so there is one
place to be wrong. Leave out version numbers, dates, and counts that will
rot. Plain sentences; sentence-case headings; no frontmatter.

`just docs-check` fails on a dead link, a dead anchor, a `phux` or `just`
command in a code block that does not exist, or a crate missing from the
architecture page.

Public behavior changes update their live task/interface home together
with relevant tests, examples and generated references. Keep generated
CLI/config/schema facts generated; keep each source-ownership map and
its rationale in one home, linked from task entry points. A new guide must
be discoverable from `docs/README.md` and its task parent; update the
website consumer's navigation/routes when pages or anchors change.

A future decision records a short reason and PR in its live home; a
constraint that closes off a design space is also linked from
[architecture: decisions](./docs/architecture.md#decisions). A bug fix is
not a new decision. Query [history](./docs/architecture.md#historical-decisions)
deliberately; still-current invariants stay live, not only in Git. No
parallel narrative for every function or TODO ledger is needed.

## Not accepted

- An embedded scripting language. Commands are typed messages; write a
  script and call `phux`.
- An in-process plugin host. Plugins are external processes declared in
  config.
- A homegrown selection or extraction engine. That is libghostty's; phux
  owns find-in-scrollback because libghostty has no search.
- Homegrown crypto. TLS, SSH, and Unix permissions.
- tmux behavior "for compatibility" where phux has a reason to differ.

## Git

Run `just hooks-install` once per clone. The staged-file budget is 5,000
tokens (bytes/4). During a real merge, only a byte-identical blob at the
same path in an actual merge parent is inherited. Edited parent files,
manual resolutions, and automatic combinations still meet the limit;
parent size is not a waiver. Split by responsibility, never strip comments
or mark authored content generated to evade the gate.

- Linear history on `main`: rebase and fast-forward, never `--no-ff`.
- One commit per task; squash before merge. The squashed subject is what
  Release Please reads: `feat:` bumps the minor, `fix:` the patch, and a
  non-conventional subject is left out of the changelog.
- `commitlint` checks every commit in the PR and the title against
  [`commitlint.config.mjs`](./commitlint.config.mjs). Never `--no-verify`.
- Draft PRs skip the compile lanes; `commitlint` still runs.
- Work in a branch and a linked worktree, never on `main` in the primary
  checkout. Build and test recipes lease a worktree to one agent session at
  a time ([development](./docs/development.md#worktrees)).

## Before you open the PR

- Did the public API change? Are the doc comments right?
- Did wire bytes change? Could it have been a capability instead of a
  version bump?
- Did a doc gain a sentence about behavior that does not exist yet?
- Is there a simpler shape with fewer abstractions?
- Could someone read this cold and understand it?
