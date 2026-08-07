---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-08-02
---

# phux deprecations reference

**TL;DR.** Deprecated spellings the current binary still accepts, each pinned with its replacement and lifecycle releases; empty when nothing is currently deprecated. Every row still parses, warns once on stderr with its replacement, is hidden from help and completions, and is scheduled for removal one release cycle or more after deprecation.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

Every deprecated spelling this build of the binary still accepts. Each one parses with its full argument surface and runs its replacement's implementation; the differences from the old behavior are exactly three, and a binary-level test pins each of them per row:

1. one warning line on stderr naming the replacement — suppressed under `--json`, where stdout carries only the document and stderr is reserved for the one-line error contract;
2. absence from `--help`;
3. absence from the generated shell completions.

A deprecated spelling survives at least one full release cycle with the warning in place; the planned-removal release is the earliest it can disappear. Move scripts to the replacement before then.

No spelling is currently deprecated. When one is added to `crate::deprecations::DEPRECATED`, it appears here as a row of this table:

| Deprecated spelling | Use instead | Deprecated in | Planned removal |
|---|---|---|---|
