---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-09-15
---

# phux deprecations reference

**TL;DR.** Deprecated spellings the current binary still accepts, each pinned with its replacement and lifecycle releases; empty when nothing is currently deprecated.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

No spelling is currently deprecated.

When one is, it keeps parsing with its full argument surface and runs its replacement's implementation, with three differences: one warning line on stderr naming the replacement (suppressed under `--json`), absence from `--help`, and absence from the generated shell completions. A deprecated spelling survives at least one full release cycle with the warning in place before it is removed.
