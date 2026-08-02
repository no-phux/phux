---
audience: contributors
stability: stable
last-reviewed: 2026-08-02
---

# 0065 — One CLI grammar

**TL;DR.** `--socket` is one root-level clap global instead of 36 hand-copied
per-verb flags; `--json` stays verb-scoped. The root's
`args_conflicts_with_subcommands` is replaced by explicit post-parse checks.
Alias parity (`ls`/`list`, `rm`/`remove`) everywhere, one `--split` flag,
one JSON error shape, and no `-j` short flag.

Status: Accepted
Date: 2026-08-02

## Context

The CLI grew one verb at a time and its grammar shows it: `--socket` was
copy-pasted 36 times (only `tag` had a subtree global), `--json` appears on
~32 verbs with no shared declaration, eight list/remove registries disagree
on alias spellings, `spawn`/`launch`/`play` take `--split h|v` while
`insert-pane`/`move-pane` take boolean `--horizontal`/`--vertical` (and
discarded `--horizontal`), and a `--json` verb failing against a dead socket
printed prose to stderr with nothing machine-readable anywhere. Each copy is
a drift risk and every inconsistency is a lesson an agent or user has to
learn twice.

## Decision

1. **`--socket` is a true clap global on the root `Cli`** — one declaration,
   `global = true`; all per-verb copies (including `tag`'s subtree global,
   `service install`'s field, and the `agent`/`config` action copies) are
   deleted. `phux --socket X ls` and `phux ls --socket X` are the same
   invocation. Verbs that never dial a server (`pair`, `plugin`, `remote`,
   `relay`, `satellite`, `enroll`, `completion`, `logs`, the local `config`
   and `workspace inspect` actions, most of `service`) refuse a provided
   `--socket` with a one-line teaching error (`commands::socketless_verb`).
2. **`args_conflicts_with_subcommands` is removed from the root.** The
   planning premise "clap globals parse both positions with zero teaching
   machinery" is false while that setting is on: clap 4.5 errors on any
   matched root arg followed by a subcommand, with no exemption for global
   args (clap_builder 4.5.44 `parser.rs:480`/`:530`). Its one job — keeping
   the root `--rec` pair on the naked `phux` attach — moves to an explicit
   post-parse check that refuses `--rec`/`--rec-format` alongside any
   subcommand, naming `phux attach --rec` and `phux rec` as the remedies.
3. **`--json` stays verb-scoped.** Help stays honest by construction: a
   global `--json` would advertise itself on verbs that cannot honor it. The
   scoped-flag rationale that used to justify per-verb `--socket` copies is
   kept for `--rec` and `--json`, and inverted for `--socket` (see Why). A
   shared flattened `JsonOpt` struct unifies the declaration (task D2).
   Misplaced `phux --json ls` gets a `Cli::try_parse` interception: clap's
   refusal plus a hint to place the flag after the verb, produced for any
   long flag that exists on some verb in the tree.
4. **JSON error contract.** Codifies the shape `spatial.rs` already emits,
   extended: one line of JSON on stderr —
   `{"schema_version": N, "error": {"code", "message"}, "remedy", "exit_code"}`
   — with stdout empty and exit codes unchanged (0 success, 1 miss/no
   server, 2 refusal/usage, 3 partial view, 124/125 timeouts). Rolled out by
   tasks D2/D3.
5. **Alias parity.** Every list/remove registry gets `ls`+`list` and
   `rm`+`remove` as visible aliases; `plugin unlink` gains `rm`/`remove`;
   `tag` gains `--json`; `launch --list` stays a flag (it filters a verb,
   not a registry). Rolled out by task D3.
6. **`--split` unification.** `insert-pane`/`move-pane` adopt `--split`
   (value enum, `h`/`v` aliases) with the booleans hidden-deprecated for one
   release; `service install --quic` unifies to `SocketAddr` to match
   `server --quic`. Rolled out by task D4.
7. **No `-j` short flag for `--json` — considered and rejected.** The
   binary has 10 short flags total, all high-frequency human-typed
   (`-o`, `-s`, `-c`, `-n`, `-f`, `-e`). `--json` is overwhelmingly typed
   by scripts and agents, where explicitness is worth more than two saved
   characters and nothing is retyped interactively. Adopting `-j` on ~32
   verbs would quadruple the short-flag surface for zero scripting gain and
   spend the letter forever. Revisit only with evidence of interactive use.

## Why

The per-verb copies optimized for honest help but produced drift: three
different `--socket` doc strings, one subtree accidentally global, and a
flag missing from newer verbs. For a flag that (a) means the same thing
everywhere, (b) is consumed by 36 of 41 verbs, and (c) is set once per
environment rather than per call, the drift cost outweighs the honesty
cost — so `--socket` inverts to global while `--rec` and `--json`, which
are genuinely per-verb semantics, stay scoped. The socketless teaching
error restores the honesty the global gives up: the flag parses everywhere,
but a verb that cannot honor it says so instead of shrugging.

## Tradeoffs

- `phux pair --help` now shows a `--socket` it will refuse at runtime; the
  refusal message is the compensation.
- Post-parse checks are code where a clap setting used to be declaration;
  both checks are pinned by regression tests
  (`root_rec_before_a_verb_is_refused_post_parse`,
  `socketless_verbs_are_named_and_socket_consumers_are_not`).
- `attach --quic`/`--ws` versus `--socket` exclusion moved from a clap
  `conflicts_with` to a runtime check, because clap validates conflicts per
  parser and a root-matched `--socket` never meets a sub-matched `--quic`.
- Deprecated boolean split flags linger hidden for a release.

## Alternatives

**Keep per-verb `--socket` copies, add a lint.** A grep-based CI check could
pin the 36 copies to one spelling, but it cannot make `phux --socket X ls`
parse, which is the position every other multiplexer accepts.

**Make `--json` global too.** Symmetric, but wrong: a third of the surface
has no JSON projection, and clap cannot express per-verb `requires` against
a root global (the same per-parser limit as the conflicts above).

**Keep `args_conflicts_with_subcommands` and special-case `--socket` in a
pre-parse argv rewrite.** Reordering argv before clap sees it hides the real
grammar from `--help`, completions, and every future maintainer.

**Adopt `-j`.** Rejected above (Decision 7); recorded here so the audit
finding has a deliberate answer rather than an omission.
