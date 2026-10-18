---
audience: contributors
stability: stable
last-reviewed: 2026-10-17
---

# 0184 — Build and CI logic is one Bun package

**TL;DR.** Every piece of build, release and CI *logic* lives in `tools/`,
one TypeScript package run by the pinned Bun: `bun tools/main.ts
<command>`. Facts Cargo holds are read from the manifests, never copied
into a table. No build or CI decision is made in Python any more; it
remains where a test fixture, example, dev benchmark or the demo setup
scripts are themselves Python. Shell stays for sequencing programs.

Status: Accepted
Date: 2026-10-17

## Context

Routing, release manifests, publish planning, validation receipts, lockfile
sync, the instruction-count comparator, workflow policy and the browser
harness had grown as about forty Python files plus a handful of Node `.mjs`
checkers, each with its own bash shim, test runner and argument
conventions. The classifier listed the web and desktop lanes' crate
closures by hand, and a test re-derived them from the manifests to catch
drift: every new crate had to be added in two places (`quictun`, most
recently, conflicted with an open routing PR). Several workflows also
embedded Python heredocs.

The repository already had one first-class scripting stack: the desktop
tooling is TypeScript on the pinned Bun, typechecked with `tsc` and linted
with type-aware `oxlint`, and the workflow gate already installs that Bun.

## Decision

1. **One package.** `tools/` is a private Bun package. `main.ts` lists the
   commands; each area (`routing`, `release`, `cargo`, `perf`, `web`, ...)
   exports its own from `src/<area>/commands.ts`, with tests beside the
   code. `main.ts` owns usage, `--help` and exit codes for all of them: a
   command throws `UsageError` (exit 2) or `Failure` (exit 1).
2. **Runtime is Bun and its built-ins.** Commands import only Bun and
   `node:` modules (TOML, YAML, globbing, `fetch`, subprocesses and the
   test runner are built in), so running one needs no `bun install`.
   `devDependencies` are the typecheck, lint and format tools.
3. **Cargo is the source of crate facts.** `src/lib/cargo.ts` reads the
   path-dependency graph from the manifests. The web and desktop lanes
   route the closure of what their builds compile (`LANE_BUILDS` in
   `src/routing/routes.ts`), so a dependency added or dropped re-routes
   without an edit.
4. **One gate.** `just tools-check` (in `just workflow-check`) runs format
   check, `tsc`, `oxlint --type-aware` and `bun test` with the desktop
   tooling's strictness.
5. **CI gets Bun from `.github/actions/setup-bun`**, which installs the
   `mise.toml` pin. The classify step on every workflow costs a Bun
   download, not a compiler.
6. **Shell stays shell.** A recipe or step that only sequences programs is
   not ported; anything with branching logic, parsing or a test is.

## Why

- One language, one runner, one test harness and one lint bar replace
  three of each, and type checking catches the shape errors the Python
  scripts found at run time.
- Reading the graph removes the hand-kept crate lists and the test that
  existed only to police them.
- Bun starts in milliseconds on a bare runner. The classifier runs about
  ninety times faster than the Python it replaces over the same 2,491 paths.

## Tradeoffs

- **TypeScript for a Rust project's build logic.** Contributors who only
  write Rust read another language. It is the one the desktop and web
  clients and the agent integrations already use.
- **The graph reader covers path edges only.** It does not resolve
  registry versions or features; routing does not need them, and
  `cargo tree` still answers feature questions (`build-features-check`).
- **Bun on every classify job.** About two seconds per workflow head.
- **Python stays installed.** Rust tests drive Python fixtures (an ACP
  peer, FFI live tests), and examples and dev benchmarks are Python, so
  the environments keep the interpreter; CI logic just no longer needs it.

## Alternatives

- **A Rust `xtask` crate.** Rejected: every workflow's first job would
  need the pinned toolchain and a cold compile (the repository's Cargo
  config also requires `mold` on Linux), or a cached binary keyed to the
  lockfile, before it could route anything. The routing logic needs none
  of Rust's strengths.
- **Keep Python, add `cargo metadata`.** Rejected: it fixes the hand-kept
  lists but leaves three scripting stacks, and `cargo metadata` would
  need a Rust toolchain on the classify runner.

## Related

- [`tools/README.md`](../../tools/README.md) is the package's layout and
  rules; [CI](../CI.md) describes the routing it implements.
- Builds on [ADR-0176](./0176-the-release-pipeline.md), whose release
  steps now call `tools` commands.
