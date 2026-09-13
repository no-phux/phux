---
audience: contributors
stability: stable
last-reviewed: 2026-07-11
---

# 0042 — Launch executor: a CLI verb that spawns an integration template

**TL;DR.** `phux launch <integration>` resolves a named agent integration
template's `[launch]` command from an enabled plugin's `integrations/`
directory, expands the `${PHUX_PLUGIN_ROOT}` placeholder into an absolute
argv, and spawns a pane running it through the existing `SPAWN_TERMINAL`
path. `[launch]` stays a field of the integration-template file (not the
`phux-plugin.toml` manifest); execution is child-process argv only — no
in-process host, no shell evaluation.

Status: Accepted
Date: 2026-07-11

## Context

The agent-tools plugin ships integration templates
(`integrations/<id>.toml`) that each declare a `[launch]` command wrapping
a real agent (claude/codex/gemini) through `phux-agent-wrap.sh`, so the
pane self-declares a `phux.agent/v1` identity (ADR-0040). The r82.11 review
flagged the gap: nothing in phux read or ran that `command`, so installing
the plugin did not make an agent self-identify — a user still had to hand-run
the wrapper or alias it. This ADR closes the design space of *what runs the
template's `[launch]` command, and where that logic lives*, under the
no-in-process-host constraint (vision.md / CONTRIBUTING.md).

## Decision

- **A CLI verb, `phux launch <integration> [-- extra args]`.** It resolves
  the integration to an argv and spawns a pane via the ordinary
  `SPAWN_TERMINAL` wire op — the same path `phux spawn` uses. No new wire
  frame; the server injects `PHUX_TERMINAL_ID` into the spawned pane exactly
  as for any spawn, so the wrapper self-targets with zero extra config.
- **`[launch]` stays in the integration template, not the manifest.**
  Integration templates are already a distinct, documented file format from
  `phux-plugin.toml`. The launch executor resolves `<id>` by scanning each
  *enabled* plugin's `integrations/*.toml` (a convention, not a
  manifest-declared path) and matching the parsed `id`. A new typed model in
  `phux-config` (`integration.rs`) parses only the launcher-relevant fields
  (`id`, `display_name`, `kind`, `[launch]`) and ignores the rest, so the
  template stays a rich, forward-compatible package.
- **`${PHUX_PLUGIN_ROOT}` expands to an absolute path before spawn.** The
  pane runs the argv directly (no shell), so the placeholder is substituted
  per argv element into the owning plugin's root. This is a plain string
  replace — never `eval`/`sh -c` — so an untrusted field cannot inject extra
  arguments or commands.
- **`working_directory` decides the pane's cwd**, defaulting to `workspace`
  (the directory `phux launch` was invoked from) so an agent runs where the
  human is; `plugin-root` is available for programs that belong to the
  plugin tree.
- **Resolution is server-free and testable.** `phux launch --print` (a dry
  run) resolves and prints the argv without spawning; `phux launch --list`
  enumerates launchable integrations across enabled plugins.

## Why

Reusing `SPAWN_TERMINAL` means the launcher composes for free with
`PHUX_TERMINAL_ID` injection (phux-w7mj) and pane recording — a launched
agent self-identifies end-to-end with no new server surface and no wire
change. Keeping `[launch]` in the template (rather than promoting it into
the plugin manifest) matches the existing split: a plugin *ships* templates,
and templates are versioned packages describing an external agent, not phux
entrypoints. Absolute-path expansion at resolution time is what lets the
wrapper script be found from any cwd while the agent still runs in the
user's workspace — the exact wart the templates worked around before a
launcher existed.

## Tradeoffs

- Scanning `integrations/` per enabled plugin is O(templates) filesystem
  work on each launch; the directories are tiny, and only enabled plugins
  are scanned.
- A broken sibling template is skipped so it cannot block a healthy launch;
  a parse error only surfaces when the requested id's own file fails to
  parse. That trades a silent skip for robustness.
- `[launch]` is not validated by `phux plugin validate` (that verb reads
  `phux-plugin.toml`, not templates); the launcher validates on resolution,
  and the demo plugin's `validate-integrations` action still checks the
  key's presence.

## Alternatives

**Promote `[launch]` into `phux-plugin.toml` as a first-class manifest
section.** Rejected: it would fold a versioned external-agent package format
into the phux plugin manifest, duplicate the template's other sections
(`[detect]`, `[session_identity]`, ...), and couple manifest schema changes
to agent packaging. The template already owns this shape.

**A `[[panes]]`-style manifest entrypoint the server auto-runs.** Rejected:
that drifts toward phux executing plugin-declared programs implicitly; an
explicit `phux launch` verb keeps the human (or an agent) in control of when
a pane is spawned and composes with the palette/keybindings like any verb.

**Shell-expand the `[launch]` command (`sh -c`).** Rejected: it opens a
shell-injection surface over template fields for no benefit — the argv is
already structured, and per-element placeholder substitution covers the one
variable the templates need.
