---
audience: contributors, agents
stability: stable
last-reviewed: 2026-05-28
---

# Contributing to phux

**TL;DR.** Pass `just ci` before opening a PR; update `docs/spec/` +
CHANGELOG for wire changes; write an ADR for any decision that closes
off design space; no homegrown crypto, scripting language, plugin
host, tmux-style copy-mode clone, or template DSL. Doc conventions live in
[`docs/CONVENTIONS.md`](./docs/CONVENTIONS.md). The mental model is
[`docs/CONCEPTS.md`](./docs/CONCEPTS.md).

phux is an experiment in building the terminal multiplexer that would
exist if libghostty had been available in 2007. We are picky about
contributions for a concrete reason: the multiplexers before us each
grew a scripting language, a plugin host, a config DSL, and a copy-mode
of their own, and the accreted surface is now the thing nobody can
finish refactoring. Every feature we decline is one we never have to
keep working across every future version of the wire.

The yardstick is the [smol manifesto](https://smol.tauri.app/): solve a
well-defined problem; behave the way users expect; be maintainable by
one person; compose with other tools; be finishable. If a proposal moves
phux away from any of those, it's the wrong proposal.

## Bar for any change

A PR must pass:

```sh
just ci
```

That target runs (and you must, locally, before pushing):

1. `cargo fmt --all --check` — formatting is mechanical, not a style debate.
2. `cargo clippy --all-targets --all-features -- -D warnings` — clippy is
   pedantic on purpose. If a lint is wrong for our case, allow it with a
   comment explaining why.
3. `cargo nextest run --workspace --all-features` — all tests pass.
4. `cargo deny check` — licenses, advisories, unauthorized sources.

## Additional expectations

- **Test what you change.** Protocol changes need `proptest` roundtrip
  cases and `insta` snapshots. State-machine changes need explicit
  transition tests. Bug fixes get a regression test, named after the
  issue.
- **Update [`docs/spec/`](./docs/spec/) when the wire changes.** The spec is
  normative — code conforms to it, not the other way around. Bump the
  protocol version per the rules in [`docs/spec/proto.md`](./docs/spec/proto.md) §6
  and append an entry to [`docs/spec/CHANGELOG.md`](./docs/spec/CHANGELOG.md).
- **Write an ADR for any decision that closes off a design space.** See
  [`ADR/README.md`](./ADR/README.md). You do not need an ADR for a bug
  fix; you do for "should this be in `core` or `server`?"
- **Public APIs are documented.** Workspace lints warn on missing docs
  for library crates. The binary crate is exempt.
- **`unsafe` requires justification.** Every `unsafe` block carries a
  `// SAFETY: …` comment naming the invariant it relies on. We prefer
  zero `unsafe` and lint for it (`#![forbid(unsafe_code)]` is the default
  for new modules unless explicitly opted out).
- **No new dependencies without a paragraph of justification in the PR.**
  Every dep is a long-term maintenance cost. Inline what you can.

## Things we will not accept

Asking saves us both time:

- **An embedded scripting language.** Commands are typed IPC messages.
  If you want logic, write a script and shell out.
- **A plugin system on day one.** Hooks are typed events. We may design a
  proper plugin contract later, after we know what is actually pluggable.
- **A homegrown selection engine.** Selection and copy delegate: text
  selection (word/line/output boundaries, OSC-133-aware) and extraction
  (plain/VT/HTML) belong to the host terminal and to libghostty-vt's
  Selection + Formatter APIs (Ghostty PR \#12794), never reimplemented
  here. phux may provide a client-local copy-mode projection over the
  focused pane: cursor movement, viewport scrolling, and highlight
  rendering are UI navigation over libghostty state, not a second
  selection model. phux also owns find-in-scrollback (`phux-server`'s
  `search` module), a literal search over the scrollback rows we already
  mirror — libghostty exposes no search or regex, so that locating step is
  ours. Search produces match coordinates and hands them to libghostty for
  extraction; it does not reimplement word/output boundaries or mouse drag
  selection.
- **Homegrown crypto.** SSH and Unix socket perms are the model.
- **"Just supporting tmux's behavior here for compatibility."** We are
  not tmux. We will be better in places and different in others, and we
  document the differences.

If your change conflicts with these, open a [Discussion] before a PR.

[Discussion]: https://github.com/phall1/phux/discussions

## Git workflow

- **Start new work from a fresh `origin/main`.** Local `main` drifts
  behind the remote (an agent once branched off a local `main` that sat
  156 commits stale, basing new work on dead code). Fetch first and
  branch off the remote tip — never off local `main`:
  ```bash
  just new-branch <branch-name>   # git fetch origin && git switch -c <branch-name> origin/main
  ```
- **Linear history is the default.** Prefer fast-forward merges or
  rebases. Do NOT create merge commits with `--no-ff` on `main` — the
  log must stay linear and bisect-friendly. For a multi-branch
  integration, the canonical sequence is:
  ```bash
  for branch in <ordered list>; do
      git rebase main "$branch"      # replay onto current main
      git checkout main
      git merge --ff-only "$branch"
  done
  ```
- **One commit per task.** Squash WIP commits before merge. The
  commit message tells the story of the change, not the keystrokes
  that produced it.
- **The squashed subject is what release-please reads.** Releases are cut
  from the conventional-commit log on `main` (see
  [`docs/RELEASING.md`](./docs/RELEASING.md)), so the *squashed* subject —
  not the WIP messages underneath it — decides the version bump and the
  changelog entry. A `feat:` bumps the minor, a `fix:` the patch, and a
  non-conventional subject is silently omitted from both.
- **Conventional commits are machine-enforced.** The `commitlint` check
  (required by main's ruleset) lints every commit in a PR *and* the PR
  title against [`commitlint.config.mjs`](./commitlint.config.mjs). A PR
  cannot merge until both conform, closing the "silently omitted from the
  release" hole above. Subjects may run to 120 chars; body lines are
  unlimited.
- **Never `--no-verify`.** Pre-commit hooks are load-bearing. If a
  hook fails, fix the root cause.
- **Draft PRs skip the compile lanes.** `check`/`test` do not run until
  the PR is marked "Ready for review" (that event triggers them), so use
  drafts freely for work-in-progress without burning CI. The `commitlint`
  gate still runs on drafts — message feedback is cheap and better early.

## Multi-agent fan-out

When fanning out parallel agent work (e.g. four agents in wave 1 of
the protocol epic):

1. **Pre-create explicit worktrees** off a fresh remote tip before
   launching agents (local `main` drifts — see the Git workflow section):
   ```bash
   git fetch origin
   git worktree add /tmp/phux-<wave>-<task> -b <branch-name> origin/main
   ```
   Do NOT rely on the Claude Code Agent tool's `isolation: worktree`
   flag for parallel launches — in wave 1 only 2 of 4 agents got real
   worktrees (race condition); the other 2 shared the main checkout.
   Self-managed worktrees are race-free.
2. **Pre-scaffold shared files** (e.g. `mod.rs`, `lib.rs`) so each
   agent owns disjoint files. This is how wave 1 avoided merge
   conflicts on `crates/phux-protocol/src/input/mod.rs`.
3. **Each agent's prompt MUST start with** a `cd /tmp/phux-...; pwd`
   to verify they're in their worktree, and an instruction to produce
   **one squashed commit** on their branch.
4. **Integration uses rebase + ff-only merge** per the Git workflow
   section above.
5. **Clean up after merge**:
   ```bash
   git worktree remove /tmp/phux-<wave>-<task>
   git branch -d <branch-name>
   ```

## Observability: CI itself

CI is instrumented end to end (ADR-0047). Where to look, cheapest first:

- **Any run's step summary** — every lane renders its cargo phase timings,
  rust-cache hit/miss, and slowest tests right on the run page.
- **`just ci-report`** — prints the recorded dashboard: per-workflow medians
  and p95s, slowest steps, cache hit rates, cold-build and binary-size
  trends. Rendered live at <https://phux.phall.io/ci> as well.
- **The `ci-metrics` branch** — the raw NDJSON store (one shard per month),
  written only by the `ci-metrics` workflow. Query it with `git show
  origin/ci-metrics:runs/<YYYY-MM>.ndjson | jq ...`.
- **The `observatory` workflow** — dispatch it when a build feels slow: cold
  dev/release `--timings` timelines (HTML artifact + distilled tables),
  binary size with bloat attribution, dependency stats. Runs weekly and on
  lockfile/toolchain changes to main, never on the PR path.
- Locally: `just timings`, `just llvm-lines`, `just bloat`, `just dep-stats`.

## Observability: tokio-console

`phux-server` has an opt-in `tokio-console` cargo feature that attaches
the [tokio-console](https://github.com/tokio-rs/console) debugger to a
running server — handy for inspecting broadcast lag, task stalls, and
poll counts in the actor system. Requires Tokio built with
`--cfg tokio_unstable`:

```sh
RUSTFLAGS='--cfg tokio_unstable' cargo run --features phux-server/tokio-console -- server
# in another shell:
cargo install --locked tokio-console
tokio-console   # connects to 127.0.0.1:6669 by default
```

## Heap profiling: dhat

The `phux` binary has an opt-in `dhat-heap` cargo feature that swaps in
the [dhat](https://docs.rs/dhat) allocator and installs a heap profiler
for the lifetime of `main()`. On clean shutdown, a `dhat-heap.json`
report is written to the current working directory:

```sh
cargo run --features dhat-heap -- server
# Ctrl-C the server to flush; then open dhat-heap.json at:
#   https://nnethercote.github.io/dh_view/dh_view.html
```

The instrumented allocator is significantly slower than the system
allocator — use for profiling only, never for production builds.

## Reviewing your own work before opening a PR

- Did the public API change? Rustdoc updated?
- Did wire bytes change? `docs/spec/` updated and CHANGELOG appended? Version bumped?
- Could this be tested with `proptest`? Probably should be.
- Is there a simpler shape with fewer abstractions? Prefer it.
- Could a future contributor read this code cold and understand it?

If you can answer "yes" to all of those, ship it.
