---
audience: humans, agents, contributors
stability: evolving
last-reviewed: 2026-08-02
---

# phux reference

**TL;DR.** Generated reference documentation, rendered from the compiled binary and byte-pinned by a freshness test so it cannot drift from the code. The table below routes to each page; none of these files is edited by hand.

<!--
GENERATED FILE - do not edit. A unit test byte-compares this page
against `phux gen-reference-docs` output and fails on any drift, so
hand edits do not survive. Regenerate with `just docs-gen`.
-->

Every file in this directory is rendered from the compiled `phux` binary by `just docs-gen`; none of it is written by hand. A unit test re-renders each page and byte-compares it against this tree, so the reference cannot drift from the binary without failing the test suite.

| Page | Contents |
|---|---|
| [cli.md](cli.md) | Every non-hidden `phux` invocation path with its flags, defaults, and help text. |
| [config.md](config.md) | Every `config.toml` section, the scalar defaults, and the annotated default config. |
| [actions.md](actions.md) | Every dispatcher action with its parameters, description, and palette placement. |
| [widgets.md](widgets.md) | Every registered status-bar widget kind with its options and defaults. |
| [hooks.md](hooks.md) | Every hook event the server fires, with its context keys, environment projection, and matching rules. |
| [exit-codes.md](exit-codes.md) | Every exit code the binary uses and what each one means. |
| [files.md](files.md) | The symbolic path rule for every file phux reads or writes: socket, config, logs, TLS material, tokens. |
| [deprecations.md](deprecations.md) | Every deprecated spelling, its replacement, and its removal release. |
