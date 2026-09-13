---
audience: contributors
stability: stable
last-reviewed: 2026-07-09
---

# 0039 — Layered config: an ordered `extends` stack with explicit array append

**TL;DR.** The two-layer merge (embedded defaults under the user file)
becomes an ordered stack: defaults, then layers named by a top-level
`extends = ["path-or-name"]` key, then the user file. Layers may extend
further layers up to a small fixed depth; cycles and missing files are
hard errors naming the offending file. Arrays still replace wholesale;
a `-append` key suffix opts a specific array into appending.

Status: Accepted
Date: 2026-07-09

## Context

[ADR-0023](./0023-config-ux-philosophy.md) committed phux to
pure-config: an embedded `default.toml` is the live base layer and the
user's `config.toml` is a sparse overlay merged leaf-by-leaf on top.
That merge supports exactly two layers. A curated distribution — a
lazyvim-style starter that ships opinionated keybindings, a status-bar
arrangement, and a plugin set — has nowhere to sit: it must either
replace the user's file (freezing the distro at copy time, the same
anti-pattern ADR-0023 rejects for scaffolds) or be pasted inline and
drift. Separately, whole-array replacement means any layer that touches
`[[plugins]]` or a status slot silently erases what the layer below
contributed, so even a hand-rolled include mechanism could not compose
plugin sets.

## Decision

1. **Ordered layer stack.** Effective config = embedded `default.toml`
   <- extended layers (depth-first, in listed order) <- the declaring
   file. Later layers win per leaf; tables merge recursively as before.

2. **`extends` is an explicit top-level key**, an array of strings in
   any config file. An entry containing a path separator or ending in
   `.toml` is a path, resolved relative to the directory of the file
   that declares it (absolute paths pass through). A bare name `n`
   resolves to `layers/n.toml` beside the declaring file. `extends` is
   consumed during layer resolution and never reaches the typed schema;
   `parse_str` (no defaults, no I/O) continues to reject it as an
   unknown field.

3. **Recursion is bounded and acyclic.** Layers may themselves declare
   `extends`, at most 4 levels below the user file. A file already on
   the current resolution chain is a cycle error; a file already merged
   via another branch (diamond) is skipped, so its values and appends
   apply exactly once. Every resolution error — unreadable layer, bad
   `extends` value, cycle, depth — names the offending file.

4. **Arrays replace wholesale by default; `-append` opts in.** A key
   `x-append` whose value is an array appends its elements to the layer
   stack's current value of `x` (creating it when absent). This exists
   for the arrays a distro must *contribute to* rather than own:
   `[[plugins]]` and `[[satellites]]` (a distro adds manifests without
   erasing the user's or a lower layer's), `status.left/center/right`
   (add a widget beside inherited ones), and `[[hooks.<name>]]` (add a
   hook without clobbering lower-layer hooks for the same event).
   Keybindings need no append mechanism: `prefix-table` and `global`
   are tables and already merge per chord. Appending to a non-array,
   a non-array `-append` value, or setting both `x` and `x-append` in
   the same file is an error naming that file. The `-append` suffix is
   reserved at every table level; a free-form key (for example a
   `[theme]` slot) may not end in `-append`.

## Why

An `extends` stack preserves ADR-0023's core property at every level:
each file carries only overrides, so a distro update reaches its users
the same way a phux default change does, and the user file stays a
sparse diff on top of the distro. Making the reference explicit and
top-level keeps "what is my config" answerable by reading one file.

Append-by-suffix rather than merging all arrays keeps replacement — the
predictable, documented behavior — as the default, while giving the
composition-shaped arrays a way to compose. The suffix is visible in
the file itself, so intent survives copy-paste; no schema annotation or
out-of-band mode switch is needed. The depth cap and cycle guard exist
because config loading runs on every CLI invocation and must fail fast
and legibly, not walk an unbounded include graph.

## Tradeoffs

- **No element identity, so no override-in-place or removal.** Append
  can only add; a user who wants to drop a distro's plugin or widget
  must replace the whole array (plain `x =` wins over inherited
  appends). Element-level identity (merge by plugin id) is deliberately
  out of scope.
- **A reserved key suffix.** `-append` is claimed at every table depth,
  which constrains free-form key/value tables like `[theme]`.
- **`phux config show` output grows less obvious.** The effective merge
  now spans N files; `show` still renders the final table but cannot
  attribute a value to a layer. Attribution shipped as
  `phux config show --layers` (phux-r82.4).
- **Diamond dedupe is positional.** A layer included twice keeps its
  first position in the stack; a later re-include cannot re-raise its
  precedence. Simple, but occasionally surprising.

## Alternatives

**Imperative mutation (`set-option`, "run this snippet to install the
distro").** Rejected in ADR-0023 and rejected again here: it forks the
source of truth and turns distro install into a one-shot copy that
drifts immediately.

**Implicit deep-merge of all arrays.** Rejected: TOML arrays have no
per-element identity, so element-wise merging either duplicates entries
(hooks, plugins listed twice) or invents matching heuristics; and it
removes the ability to *replace* a list at all, which the status-bar
slots rely on today.

**Schema-annotated merge modes** (the Rust schema marks which fields
append). Rejected: the behavior would be invisible in the file being
edited, and every schema addition would need a merge-mode decision;
the suffix keeps the semantics local to the config text.

**Distro as a replacement `default.toml`.** Rejected: it forks the
embedded defaults, loses phux-release default updates, and still
supports only one distro with no user-side stacking.
