---
audience: contributors, agents
stability: stable
last-reviewed: 2026-10-05
---

# Contributing to phux

**TL;DR.** Test the area you change and expand coverage for shared code;
use `just ci-full` for the full root PR bar. Update `docs/spec/` +
CHANGELOG for wire changes; write an ADR for any decision that closes
off design space; no homegrown crypto, scripting language, plugin
host, tmux-style copy-mode clone, or template DSL. Doc conventions and the
mental model live in `docs/CONVENTIONS.md` and `docs/CONCEPTS.md`.

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

## Get set up

Start with [Contributor setup](./docs/SETUP.md): `mise install` or
`nix develop`, then the same `just` recipes. A GitHub issue or PR is enough
to coordinate; you do not need Beads.

## Agent entrypoints

These are the files that configure coding agents in this repo. Everything
else at the root is product source, not an agent zoo.

| File | Job |
|---|---|
| [`AGENTS.md`](./AGENTS.md) | Shell hygiene, worktrees, beads. Most coding agents load this. |
| [`CLAUDE.md`](./CLAUDE.md) | Architecture map. Claude Code also loads this. |
| [`.claude/settings.json`](./.claude/settings.json) | Claude Code session hooks (`bd prime`). |
| [`.codex/hooks.json`](./.codex/hooks.json) | Codex session hooks (`bd codex-hook`). |
| [`.claude-plugin/marketplace.json`](./.claude-plugin/marketplace.json) | The Claude Code plugin this repo ships. |
| [`.agents/skills`](./.agents/skills) | Standard project skills: Beads workflow guidance and the canonical, versioned phux CLI/MCP guides compiled into releases. |

## Bar for any change

A PR must pass its applicable CI lanes. During development, run the scoped
checks in the setup guide. For changes spanning the root Rust/CLI surface:

```sh
just ci        # full root deterministic/unit gate set
just ci-full   # ci + the real-server lanes; the complete PR bar
```

Use scoped gates while iterating, then expand for shared code and affected
consumers. `just ci-full` adds real-server e2e and the agent example smoke; run
it before pushing anything that touches the CLI surface, server lifecycle, or
the example scripts. Cockpit and browser builds have their own additional
checks; the root target does not build those clients.

### Build only what you are iterating on

| Command | Build scope |
|---|---|
| `just build` or `cargo build` | Developer executables: `phux` and `phux-mcp` |
| `just build-lean` | Same executables without browser HTTP/3/WebTransport; UDS, WebSocket and raw QUIC remain |
| `cargo build -p phux-mcp` | Headless MCP adapter, without the client's TUI chrome |
| `just build-all` | All workspace targets, including integration tests, examples and benchmarks |
| `just build-release` | Shipping executables with full release optimization |
| `just cockpit-build`, `just cockpit-dev`, `just cockpit-test` | Cockpit with the unwind-safe, incremental `ffi-dev` profile |
| `just cockpit-node-test` | Cockpit Node TypeScript tests (`src/tests/*.test.mjs`) |
| `just cockpit-ffi-release` | Production FFI with unwind safety and full release optimization |

The default executable still supports browser WebTransport. Lean builds opt out
via `phux --no-default-features` at Cargo build time; workspace tests deliberately
enable the complete server transport surface. The TUI is its own crate
(`phux-tui`, ADR-0100), so a headless consumer such as `phux-mcp` links
`phux-client` and cannot reach the chrome. `just build-features-check`
checks these resolved dependency boundaries without compiling.
`just build-features-compile` separately type-checks the lean executable,
headless client/MCP targets, and `phux-tui --no-default-features` so workspace
feature unification cannot hide errors. Both checks run in `just ci` and CI.

Use `cargo check -p <crate>` for a targeted type check. For repeated workspace
test runs, keep the Cargo build selection as `--workspace` and select tests with
nextest's `-E` filter: changing package selection or profiling features can
produce a different dependency feature union and rebuild downstream crates.
The `just test`, `just e2e` and `just stress` recipes share that build selection.

For timing evidence, append `--timings` to a Cargo build and inspect
`target/cargo-timings/`. Keep each concurrent worktree's Cargo target
directory private; mbx shares compiled work between them through its store,
not through a shared `target/` ([setup](./docs/SETUP.md#build-cache-mbx)).

### Gate-by-gate: local vs CI

The local bar is a **superset** of `.github/workflows/ci.yml`: a gate CI runs
and `just ci` does not is a gate you discover by pushing. The workflow calls
the `just/*.just` recipes instead of re-typing cargo flags, so flags live in
one place; prefer adding a recipe to naming a bare `cargo` command in CI.
Product recipes stay out of the root `justfile` so CI can route by module.

| Gate | CI (`ci.yml`) | Local |
|---|---|---|
| formatting | `just fmt-check` | same recipe |
| clippy | `just lint` | same recipe |
| rustdoc | `just doc` | same recipe |
| dependency hygiene | `just deny` | same recipe |
| production feature boundaries | `just build-features-check` | same recipe |
| opt-out feature compilation | `just build-features-compile` | same recipe |
| doc system | `just docs-check` | same recipe |
| generated glyph table | `just font-check` | same recipe |
| e2e lane coverage | `just e2e-lane-check` | same recipe |
| Homebrew formula | `just formula-check` | same recipe |
| toolchain pins | `just toolchain-check` | same recipe; `just toolchain-parity` additionally compares the resolved Nix and Mise environments |
| unit tests | `NEXTEST_PROFILE=ci just test` (default features; PRs may pass `PHUX_NEXTEST_FILTERSET`) | `just test`; `just test-cargo` if nextest is unavailable |
| workflow/setup contracts | `just workflow-check` (includes `just shellcheck`) | same (includes `just setup-check`'s helper tests) |
| agent integration packages | `bash scripts/ci/agent-integrations.sh` | `just agent-integrations-check` (same script); `just integration-check <package>` for a scoped loop |
| Zig archive pins | `scripts/check-zig-pins.sh` | `just zig-pin-check` |
| install surface | `scripts/check-install-surface.sh` | `just install-surface-check` |
| build-cache portability | `just cache-portable-check` | same recipe |
| embedded skill contract | `just skill-contract` | same |
| fast e2e + perf gates | `just e2e` | `just e2e`, via `just ci-full` |
| agent example smoke | `just agents-fleet-smoke` | same, via `just ci-full` |

Unit and e2e tests share the default feature set; optional profiling
surfaces compile under `just lint` and `just doc` with `--all-features`.
`just e2e` stays out of `just ci` because it spawns real PTY-backed servers
with wall-clock ceilings a loaded laptop can miss; `just ci-full` includes it.

### Gates that are CI-only or local-only

Native environment smoke runs in `native-setup.yml` (Linux; reproduce with
`just native-smoke`) and `cockpit-ci.yml` (macOS). These have no local
equivalent: the draft/docs-only `changes` routing job, caching (Cachix,
rust-cache, sccache), CI step summaries (`scripts/ci/timed.sh`, ADR-0082; use
`just dep-stats` or `just timings` locally), the post-merge/nightly `stress`
workflow (run `just stress` locally; 2-core runners starve the current-thread
runtime), commit-message linting, and the release/publish lanes
(`just release-preflight <tag>` runs their offline parts).

`just milestone-check` is local-only and advisory: it asserts every non-closed
bead carries exactly one of `rc-1.0` / `post-1.0` by querying the live Dolt
store through `bd`. CI has no store, and the check deliberately does not read
the tracked `.beads/issues.jsonl`: that file is a passive export that can lag
the store. Without `bd` or a store it prints `SKIPPED` and exits 0; an
unlabelled or double-labelled bead exits 1.

## Additional expectations

- **Test what you change.** Protocol changes need `proptest` roundtrip
  cases and `insta` snapshots. State-machine changes need explicit
  transition tests. Bug fixes get a regression test, named after the
  issue.
- **Update [`docs/spec/`](./docs/spec/) when the wire changes.** The spec is
  normative — code conforms to it, not the other way around. Bump the
  protocol version per the rules in [`docs/spec/proto.md`](./docs/spec/proto.md) §6
  and append an entry to [`docs/spec/CHANGELOG.md`](./docs/spec/CHANGELOG.md).
  A frame or field change also regenerates
  [`docs/spec/wire-schema.json`](./docs/spec/wire-schema.json)
  (`PHUX_UPDATE_WIRE_SCHEMA=1 cargo nextest run -p phux-protocol wire_schema_matches`);
  the protocol tests fail until it matches the codec.
- **Reach for a capability before you reach for a version bump.** The server
  admits a client only when `major.minor` matches exactly, so a minor bump
  breaks every deployment at once rather than degrading gracefully. New
  frames, command tags, and fields ship as a `ServerFeature` bit, a
  `ClientCapabilities` byte, or an additive field id; a version bump is for
  changes no additive shape can express, and the PR says so out loud. The
  rule is normative in [`docs/spec/proto.md`](./docs/spec/proto.md) §6.3 and
  argued in [`docs/adr/0061`](./docs/adr/0061-capabilities-add-versions-break.md).
  The `ServerFeature` u32 is closed; the next bit is the trailing word in
  [`docs/adr/0137`](./docs/adr/0137-server-feature-word-extends.md).
- **Do not document what you did not build.** In `docs/spec/` and
  `docs/consumers/`, a surface the reference implementation does not provide
  carries an `impl-status` marker naming a code symbol, and `just docs-check`
  verifies the marker against the code. See
  [`docs/CONVENTIONS.md`](./docs/CONVENTIONS.md) §"Implementation status".
- **Write an ADR for any decision that closes off a design space.** See
  [`docs/adr/README.md`](./docs/adr/README.md). You do not need an ADR for a bug
  fix; you do for "should this be in `core` or `server`?"
- **Public APIs are documented.** Workspace lints warn on missing docs
  for library crates. The binary crate is exempt.
- **Alias a wire type at the import when its bare name is taken.**
  `phux-core` holds the in-memory shape and `phux-protocol` the wire shape of
  several concepts, kept independent by
  [`docs/adr/0011`](./docs/adr/0011-protocol-core-independence.md). Where both
  are in scope, import the protocol one with a `Wire` prefix:
  `use phux_protocol::ids::ResourceId as WireResourceId;`. This matters for
  `ClientId` (a `u32` wire identity versus the server's `u64` routing id),
  `ResourceId`, `SessionId`, `WindowId`, `WindowInfo`, and
  `HistoryRejectionReason`. The split tree is not a wire type: the client's
  `LayoutNode` and `SplitDir` live in `phux-client-core::layout` (the L3
  envelope's tree) beside `phux-core`'s server-side ones.
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
- **An in-process plugin host.** Plugins are external packages declared in
  config, not code loaded into the server.
- **A homegrown selection engine.** Selection (word/line/output boundaries,
  OSC-133-aware) and extraction (plain/VT/HTML) belong to the host terminal
  and libghostty-vt's Selection + Formatter APIs. phux may provide a
  client-local copy-mode projection (cursor movement, scrolling, highlight)
  over libghostty state, and owns find-in-scrollback (`phux-server`'s
  `search` module) because libghostty has no search; matches are handed back
  to libghostty for extraction.
- **Homegrown crypto.** SSH and Unix socket perms are the model.
- **"Just supporting tmux's behavior here for compatibility."** We are
  not tmux. We will be better in places and different in others, and we
  document the differences.

If your change conflicts with these, open a [Discussion] before a PR.

[Discussion]: https://github.com/no-phux/phux/discussions

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
- **One commit per task.** Squash WIP commits before merge.
- **The squashed subject is what release-please reads** (see
  [`docs/RELEASING.md`](./docs/RELEASING.md)): `feat:` bumps the minor,
  `fix:` the patch, and a non-conventional subject is omitted from both.
- **Conventional commits are machine-enforced.** The required `commitlint`
  check lints every commit in a PR *and* the PR title against
  [`commitlint.config.mjs`](./commitlint.config.mjs). Subjects may run to 120
  chars; body lines are unlimited.
- **Never `--no-verify`.** If a hook fails, fix the root cause.
- **Draft PRs skip the compile lanes.** `check`/`test` run once the PR is
  marked ready for review; `commitlint` still runs on drafts.

## Multi-agent fan-out

1. **Pre-create explicit worktrees** before launching parallel agents
   (`git worktree add /tmp/phux-<wave>-<task> -b <branch> main`); the Agent
   tool's `isolation: worktree` flag has raced and shared the main checkout.
2. **Pre-scaffold shared files** (`mod.rs`, `lib.rs`) so each agent owns
   disjoint files.
3. Each agent verifies its worktree first and produces **one squashed commit**.
   Agents build through the environment's `cargo` (or `mise exec --` /
   `nix develop -c`) so mbx's shared store and compiler pool apply; check
   `df -h` before a wide wave all the same.
4. **Integrate with rebase + ff-only merge**, then remove the worktree and
   branch.

Shared registries are where disjoint-file merges bite: two branches can claim
the same ADR number or spec version with no textual conflict. Two registries
are enforced by `check_registry_rows` in `scripts/check-docs.sh`, which
requires unique keys and strict ordering:

| Registry | Key | Order | Gate |
|---|---|---|---|
| `docs/adr/README.md` index (see docs/CONVENTIONS.md §"The index row") | ADR number `NNNN` | ascending, and the row's link must resolve to that ADR | `adr-index-sync` |
| `docs/spec/CHANGELOG.md` | wire version, e.g. `0.9.0-draft.1` | descending, newest at the top | `spec-version-sync` |

Instantiate the helper for any new registry rather than hand-rolling a gate.
`ServerFeature` bits and command or event tags are first-come on main
([`docs/adr/0137`](./docs/adr/0137-server-feature-word-extends.md)); a branch
plan is not a reservation. Rebase and re-run the gate before declaring a
branch done.

## Observability: CI itself

CI keeps no metrics store (ADR-0082). Each run's step summary shows cargo
phase timings, cache hits, target size, and slowest tests; locally use
`just timings`, `just llvm-lines`, `just bloat`, and `just dep-stats`.

## Profiling

`phux-server`'s opt-in `tokio-console` feature attaches
[tokio-console](https://github.com/tokio-rs/console) to a running server
(broadcast lag, task stalls, poll counts):

```sh
RUSTFLAGS='--cfg tokio_unstable' cargo run --features phux-server/tokio-console -- server
tokio-console   # in another shell; connects to 127.0.0.1:6669
```

The `dhat-heap` feature builds a separate `phux-dhat-heap` executable with the
[dhat](https://docs.rs/dhat) allocator; the ordinary `phux` binary keeps the
system allocator. A clean shutdown writes `dhat-heap.json` to the working
directory (view it in dh_view). Profiling builds only; it is slow.

```sh
cargo run -p phux --features dhat-heap --bin phux-dhat-heap -- server
```

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
