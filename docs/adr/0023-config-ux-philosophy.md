---
audience: contributors
stability: stable
last-reviewed: 2026-05-29
---

# 0023 — Config UX: pure-config, defaults as a live base layer

**TL;DR.** phux is configured the Ghostty way: one TOML file, defaults
compiled into the binary, the user's `config.toml` a sparse overlay
merged on top. The shipped defaults live in an embedded, annotated
`default.toml` that *is* the base layer at runtime — not values frozen
into the user's file. `phux config init` scaffolds a fully-commented
projection of those defaults and never overwrites without `--force`;
`show` / `show --default` / `path` inspect. No imperative settings
mutation.

Status: Accepted
Date: 2026-05-29

## Context

phux needs a settings story for the reference TUI consumer: keybindings,
status bar, shell, scrollback, hooks, theme. Two broad shapes exist. One
is *imperative*: a `set-option`-style command mutates persisted state
(tmux's model), so the source of truth is an opaque runtime blob and the
file, if any, is a replay log. The other is *declarative / pure-config*:
a text file is the entire source of truth; the program reads it and never
writes settings back (Ghostty's model).

We already had the mechanism for the declarative shape without having
named the policy: `phux-config` ships an embedded, prose-annotated
`default.toml` (`DEFAULT_CONFIG_TOML`), and `parse_with_defaults` merges
the user's file *on top* of it leaf-by-leaf. What was missing: a
committed decision, a way to scaffold a starter file, and inspection
commands — plus a rule for what that scaffold contains, because the naive
"write the defaults out as values" choice quietly freezes them.

## Decision

1. **Pure-config.** All TUI-local settings are expressed in one TOML file
   at `$XDG_CONFIG_HOME/phux/config.toml` (`~/.config` fallback). phux
   never writes settings back from running state. There is no
   `set-option` verb.

2. **Defaults are a live base layer, not a template.** The shipped
   defaults are the embedded `default.toml`, merged under the user's file
   at load time. A user's `config.toml` carries *only overrides*; any key
   it omits tracks the binary's default, so changing a default in a phux
   release reaches every user who hasn't overridden that key.

3. **Scaffold = commented projection, never a value dump.** `phux config
   init` writes a copy of the embedded defaults with every active
   assignment and table header commented out (see
   `phux_config::scaffold`). It documents every option *with its real
   default visible*, parses to an empty overlay (inert), and freezes
   nothing. It refuses to overwrite an existing file without `--force`.

4. **Explicit, never automatic.** Nothing auto-writes a config. `phux
   config init` is on-demand; a `just scaffold-config` dev target writes
   into a worktree-local XDG dir for testing. Inspection is `phux config
   path` (resolved path), `phux config show` (effective merged config as
   canonical TOML), and `phux config show --default` (shipped defaults
   verbatim).

This is TUI-local: none of it touches the wire
([ADR-0017](./0017-tui-not-protocol-privileged.md)).

## Why

A single declarative file is diffable, reviewable, version-controllable,
and reproducible — the same properties that make Ghostty's config
pleasant. An imperative `set-option` surface forks the source of truth
between a file and a runtime blob and invites drift; we decline it.

The base-layer rule is the load-bearing one. Materializing defaults into
the user's file is the common scaffold anti-pattern: it pins behavior to
whatever the defaults were on install day, so a later release that
improves a default silently misses everyone who ran `init`. A commented
projection gives the same discoverability — every option, its real
default inline — while keeping the binary authoritative.

## Tradeoffs

- **No in-app settings editing.** Changing a setting means editing a file
  (then, eventually, reloading). That is the deal pure-config makes; a
  GUI/TUI settings surface, if ever wanted, must round-trip through the
  file, not a side channel.
- **Two artifacts to keep coherent:** the embedded `default.toml` and the
  typed schema. They already had to agree; this ADR adds a test that the
  scaffold projection stays inert, but the schema/`default.toml`
  agreement is still convention-plus-tests, not types.
- **`config show` reformats.** It renders the merged TOML *table*, so
  comments and key order are lost and inline tables may be expanded to
  dotted-key form. It answers "what is my effective config," not "show me
  my file" — `cat` the file for the latter.

## Alternatives

**Imperative `set-option` (tmux).** Familiar, allows live tweaking
without touching a file. Rejected: it splits the source of truth and
makes "what is my config" unanswerable from the filesystem alone.

**Scaffold materialized defaults.** Simplest to generate (serialize
`Config::default()`), and the file is immediately "complete." Rejected:
it freezes defaults at install time — the failure mode in the Decision.

**No scaffold at all (strict Ghostty).** Ghostty ships zero config and
expects the user to author one. Defensible, but a commented starter that
documents every option in place is friendlier and, because it is inert,
costs nothing in the freeze dimension.
