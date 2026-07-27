---
audience: contributors, agents
stability: stable
last-reviewed: 2026-07-27
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
just ci        # the inner loop: every deterministic gate CI runs
just ci-full   # ci + the real-server lanes; the complete PR bar
```

`just ci` is the target you run on every save-and-push. `just ci-full` adds
the two lanes that spawn real processes, and is what a PR is actually judged
on — run it before pushing anything that touches the CLI surface, the server
lifecycle, or the example scripts.

### Gate-by-gate: local vs CI

The local bar is a **superset** of `.github/workflows/ci.yml` by design. It
is not enough for `just ci` to be *similar* to CI: a gate that CI runs and
`just ci` does not is a gate you discover by pushing. PR #306 shipped five
`rustdoc::private_intra_doc_links` failures to a red build after a green
local run, because `just ci` had no rustdoc step. Every row below is a
commitment to keep the two columns aligned.

| Gate | CI (`ci.yml`) | Local |
|---|---|---|
| formatting | `cargo fmt --check` | `just fmt-check` (adds `--all`) |
| clippy | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | `just lint` (identical) |
| rustdoc | `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --workspace --all-features` | `just doc` (identical) |
| dependency hygiene | `cargo deny check` | `just deny` (identical) |
| doc system | `scripts/check-docs.sh` | `just docs-check` (identical) |
| generated glyph table | `scripts/check-generated-font.sh` | `just font-check` (identical) |
| e2e lane coverage | `scripts/check-e2e-lanes.sh` | `just e2e-lane-check` (identical) |
| Homebrew formula | `scripts/check-formula.sh` | `just formula-check` (identical) |
| unit tests | `cargo nextest run --workspace` (default features) | `just test` (adds `--all-features`) |
| fast e2e + perf gates | `just e2e` | `just e2e`, via `just ci-full` |
| agent example smoke | `just agents-fleet-smoke` | same, via `just ci-full` |

Two rows are worth reading twice:

- **Unit tests are not a strict superset in the feature dimension.** CI runs
  the *default* feature set; `just test` runs `--all-features`. Neither
  contains the other, and that is deliberate — `--all-features` enables
  `phux/dhat-heap`, which installs dhat as the global allocator and would
  make the wall-clock perf gates measure dhat instead of phux. The default
  feature set still gets compiled locally, by `just e2e`'s `--workspace`
  build (and by `just check`).
- **`just e2e` is not in `just ci`.** It spawns real PTY-backed servers and
  ends in two wall-clock ceilings, which a laptop under load can miss for
  reasons that say nothing about the diff. Keeping it out means a `just ci`
  failure always indicates a real defect; `just ci-full` is where you pay
  for the full picture.

### Gates that are CI-only, and why

These have no local equivalent and none is planned. If you are chasing a
red build in one of them, the answer is on the runner, not on your machine.

- **Draft / docs-only fast paths** (`changes` job). Pure GitHub event
  plumbing — it decides which lanes to skip based on the PR diff and draft
  state. There is nothing to run locally; locally you just run the gate.
- **Caching and build acceleration** (Cachix, `Swatinem/rust-cache`,
  sccache, the runner disk-headroom step). Runner infrastructure. A cache
  miss is a slow CI run, never a wrong one.
- **CI observability** (`scripts/ci/timed.sh`, the `lane signal` step, the
  `ci-metrics` workflow, the `observatory` cold-build timings). These write
  to a metrics branch and need the Actions API plus `contents:write`. Run
  `just dep-stats` or `just timings` locally for the same lenses; use
  `just ci-report` to read what CI recorded.
- **Heavy stress storms** (`stress` workflow, post-merge and nightly). Not
  slow because they are thorough — slow because a 2-core hosted runner
  starves the server's current-thread runtime, turning a 0.3s test into 13
  minutes. `just stress` runs them locally, where they are fast; they will
  never be a PR gate.
- **Commit-message linting** (`conventional-commits` workflow). Needs the
  PR's base/head SHAs and the PR title, which only exist server-side. Write
  conventional commits and it never fires.
- **Release and publish lanes** (`release`, `release-please`,
  `publish-crate`). Need tags, a deploy key, and a crates.io token.
  `just release-preflight <tag>` runs the parts that do not — version
  checks, install-surface drift, formula generation, and a `phux-protocol`
  package dry-run.

## Additional expectations

- **Test what you change.** Protocol changes need `proptest` roundtrip
  cases and `insta` snapshots. State-machine changes need explicit
  transition tests. Bug fixes get a regression test, named after the
  issue.
- **Update [`docs/spec/`](./docs/spec/) when the wire changes.** The spec is
  normative — code conforms to it, not the other way around. Bump the
  protocol version per the rules in [`docs/spec/proto.md`](./docs/spec/proto.md) §6
  and append an entry to [`docs/spec/CHANGELOG.md`](./docs/spec/CHANGELOG.md).
- **Reach for a capability before you reach for a version bump.** The server
  admits a client only when `major.minor` matches exactly, so a minor bump
  breaks every deployment at once rather than degrading gracefully. New
  frames, command tags, and fields ship as a `ServerFeature` bit, a
  `ClientCapabilities` byte, or an additive field id; a version bump is for
  changes no additive shape can express, and the PR says so out loud. The
  rule is normative in [`docs/spec/proto.md`](./docs/spec/proto.md) §6.3 and
  argued in [`ADR/0061`](./ADR/0061-capabilities-add-versions-break.md).
- **Do not document what you did not build.** In `docs/spec/` and
  `docs/consumers/`, a surface the reference implementation does not provide
  carries an `impl-status` marker naming a code symbol, and `just docs-check`
  verifies the marker against the code. See
  [`docs/CONVENTIONS.md`](./docs/CONVENTIONS.md) §"Implementation status".
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

1. **Pre-create explicit worktrees** before launching agents:
   ```bash
   git worktree add /tmp/phux-<wave>-<task> -b <branch-name> main
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
- Did wire bytes change? `docs/spec/` updated and CHANGELOG appended? Could
  the change have been a capability instead of a version bump (§6.3)?
- Did a doc gain a sentence about behavior that does not exist yet? It needs
  an `impl-status` marker, or it belongs in an ADR.
- Could this be tested with `proptest`? Probably should be.
- Is there a simpler shape with fewer abstractions? Prefer it.
- Could a future contributor read this code cold and understand it?

If you can answer "yes" to all of those, ship it.
