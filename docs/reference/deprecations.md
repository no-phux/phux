---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-08-02
---

# phux deprecations reference

**TL;DR.** Deprecated spellings the binary still accepts: the machine-registry verbs (`remote`, `satellite`, top-level `enroll`) absorbed into `phux host`, and the `--horizontal`/`--vertical` booleans absorbed into `--split`. Each still parses, warns once on stderr with its replacement, is hidden from help and completions, and is scheduled for removal one release cycle or more after deprecation.

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

| Deprecated spelling | Use instead | Deprecated in | Planned removal |
|---|---|---|---|
| `phux remote add` | `phux host add` | v0.10.0 | v0.12.0 |
| `phux remote list` | `phux host ls` | v0.10.0 | v0.12.0 |
| `phux remote remove` | `phux host rm` | v0.10.0 | v0.12.0 |
| `phux satellite add` | `phux host add --role satellite` | v0.10.0 | v0.12.0 |
| `phux satellite list` | `phux host ls --role satellite` | v0.10.0 | v0.12.0 |
| `phux satellite enroll` | `phux host enroll --role satellite` | v0.10.0 | v0.12.0 |
| `phux satellite remove` | `phux host rm --role satellite` | v0.10.0 | v0.12.0 |
| `phux enroll` | `phux host enroll` | v0.10.0 | v0.12.0 |
| `phux insert-pane --horizontal` | `phux insert-pane --split horizontal` | v0.9.0 | v0.12.0 |
| `phux insert-pane --vertical` | `phux insert-pane --split vertical` | v0.9.0 | v0.12.0 |
| `phux move-pane --horizontal` | `phux move-pane --split horizontal` | v0.9.0 | v0.12.0 |
| `phux move-pane --vertical` | `phux move-pane --split vertical` | v0.9.0 | v0.12.0 |

The warning is one greppable stderr line per invocation, of the form:

```text
phux: `phux remote add` is deprecated and will be removed; use `phux host add`
```
