---
audience: contributors
stability: stable
last-reviewed: 2026-09-13
---

# 0121 — The CLI parser is usage-rs

**TL;DR.** The `phux` CLI parses with usage-rs 6.9, not clap. One
declaration drives parse, help, completions, and the generated CLI
reference. The usage CLI itself is pinned at 6.9.0 in `mise.toml` and
`flake.nix` (nixpkgs lags; same digest-table pattern as Bun). `--socket`
stays a root global (ADR-0065).

Status: Accepted
Date: 2026-09-13

## Context

The binary crate was a clap derive tree: a giant `Command` enum, a giant
dispatch match, `clap_complete` pruning hacks, and custom walks for
`--help` inventory, capabilities JSON, and `docs/reference/cli.md`. Each
walk could drift from the others. mise already ships usage-rs as its
parser; the crate is a typed parser plus a portable spec, not a clap
layer.

## Decision

1. **usage-rs 6.9** (`package = "usage-rs"`, import `usage`) with the
   `completions`, `validation`, and `test` features. clap and
   `clap_complete` leave the workspace.
2. **The usage CLI is pinned at 6.9.0** in `mise.toml`; `flake.nix` reads
   that pin and fetches GitHub-release tarballs because nixpkgs is behind.
   `just toolchain-check` holds the two environments together.
3. **One `#[usage]` tree** is the source for parse, help, live
   completions (`__complete_word__`), and the generated CLI reference.
   Hidden verbs stay parseable and stay out of `--help` and completions.
4. **`--socket` remains a root global**; `--rec` / `--remote` scope
   rules stay post-parse (ADR-0065). usage-rs requires `after_long_help`
   to be a `&'static str`, so EXIT STATUS is a const twin of the
   `exit_codes` table, lockstep-tested against the renderer.
5. **Dispatch stays an explicit match** in this cutover. `RunWith` is a
   later cleanup, not a requirement of the parser change.

## Why

A portable spec is the only way help, completions, and `docs/reference/`
cannot disagree. Wrapping clap would keep three walks. Waiting for
nixpkgs would pin two environments to different usage versions.

## Tradeoffs

- Completions are runtime hooks, not baked-in verb lists. Tests query
  `__complete_word__` rather than grepping scripts.
- usage also renders a spec `Commands:` catalog under the curated
  grouped inventory. The grouped block remains the complete surface.
- Some clap arg-groups have no usage equivalent (`new --json` requires
  `-s` on the verb's own `--json`; `--qr` with `pair rotate` is refused
  post-parse).
- `after_long_help` cannot be a runtime join.

## Alternatives

**Keep clap and generate a usage spec from it.** Two sources of truth.

**Thin clap wrapper around usage types.** The clap tree stays, and so do
the walks this change exists to delete.

**Wait for nixpkgs.** mise and Nix would disagree on the usage CLI.
