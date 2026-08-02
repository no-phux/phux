---
audience: contributors
stability: stable
last-reviewed: 2026-08-02
---

# 0069 — Generated reference docs from the compiled binary

**TL;DR.** Reference pages under `docs/reference/` are rendered by the phux
binary itself through a hidden `gen-reference-docs` subcommand and kept fresh
by a unit test that byte-compares the checked-in tree against a fresh render,
failing with `just docs-gen` as the remedy. Not an xtask, not a build script,
no new CI wiring: the test is the gate.

Status: Accepted
Date: 2026-08-02

Numbering note: planning documents allocated ADR-0067 to this decision;
0067 and 0068 were taken by decisions that landed first, so it carries 0069.

## Context

The hand-maintained reference prose kept drifting from the code, in the
documents new users read first: verbs described as unbuilt that ship, widget
kinds documented that were never registered, schema overviews missing whole
sections. Truth passes fix a snapshot and then rot again, because nothing
makes the next surface change touch the prose.

Two in-tree precedents already derive user-facing surface from the compiled
binary instead of prose: `phux completion` renders shell completions from
the live clap tree, and the `help_inventory` test pins the command surface
against a checked-in snapshot. The missing piece was applying the same move
to the reference documentation itself.

## Decision

- A registry, `refdocs::pages()` in the `phux` binary crate, maps each
  generated page to a deterministic renderer over one of the binary's own
  inventories. The first page is the CLI reference (`docs/reference/cli.md`),
  a walk of the clap tree that skips hidden subcommands and embeds each
  path's verbatim long help; a generated `README.md` indexes the registry.
- A hidden subcommand, `phux gen-reference-docs [--out DIR]`, writes every
  registered page. `just docs-gen` is its porcelain.
- A unit test re-renders every page and byte-compares it against
  `docs/reference/` on disk — both directions: registered pages must match,
  and files with no registered renderer must not exist. Failures name
  `just docs-gen`. The test rides `just test` inside `just ci`; there is no
  new CI gate.
- Generated pages emit the doc-system scaffolding (frontmatter with a fixed,
  generator-owned `last-reviewed` date; a TL;DR; a GENERATED FILE marker),
  so `scripts/check-docs.sh` needs no carve-out and regeneration is
  byte-idempotent.

## Why

The generator must see the binary's real inventories, and the clap tree is
already compiled into the `phux` crate — a subcommand gets it for free. The
same registry is shared by the generator, the freshness test, and the index,
so a new page cannot be written but unchecked, or checked but unlisted.
Byte-comparison makes the gate exact: any drift is caught, and the remedy is
one command. Hiding the subcommand keeps internal tooling out of `--help`
and out of the generated reference itself, while the `help_inventory`
snapshot still pins its existence.

## Tradeoffs

- The docs generator lives inside the shipped binary. The cost is a few
  kilobytes of renderer code in release builds; gating it behind a feature
  flag would make the freshness test and dev builds diverge from releases.
- Byte-exact comparison means cosmetic generator changes rewrite whole
  pages. Accepted: regeneration is one command, and looseness is how drift
  starts.
- The fixed `last-reviewed` date must be bumped by hand when the generator
  meaningfully changes. Accepted in exchange for idempotent output.

## Alternatives

**A cargo xtask.** A separate crate would re-compile the dependency graph a
second time just to reach the clap tree the binary already has, and adds a
workspace member with no other purpose.

**A build script.** Build scripts must not write into the source tree
(Cargo's contract confines them to `OUT_DIR`), and a docs tree materialized
into `OUT_DIR` is not reviewable or linkable.

**Keep curating by hand plus periodic truth passes.** This is the status quo
that failed; a pass fixes a snapshot and nothing stops the next drift.

**A docs-check gate instead of a unit test.** Would need the compiled binary
inside a shell-script gate, adding CI wiring and a build dependency to
`docs-check`; the unit test gets the binary for free and already rides CI.
