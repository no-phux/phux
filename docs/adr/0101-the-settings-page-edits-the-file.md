---
audience: contributors
stability: stable
last-reviewed: 2026-09-08
---

# 0101 — The settings page edits the file

**TL;DR.** The TUI gets a settings page, and it is a file editor with a
schema. Every row is one key of the user's `config.toml`; every edit sets or
removes that one key in that one file, preserving every other byte, and is
validated before it is written. Running state never writes back. The
catalogue of settings is pinned to the config schema by tests, and theme
slots are contributed by the renderer that owns them.

Status: Accepted
Date: 2026-09-08

## Context

ADR-0023 chose pure-config: one TOML file is the whole source of truth,
defaults are a live base layer compiled into the binary, and phux never
writes settings back from running state. Its tradeoffs section named the
cost and the door it left open: "No in-app settings editing ... a GUI/TUI
settings surface, if ever wanted, must round-trip through the file, not a
side channel."

The surface is now wanted. phux has many knobs, spread over eight tables,
and the only ways to learn them were the annotated `default.toml`, the
generated config reference, and `phux config show --layers`. None of those
is reachable from inside an attach without leaving the pane, and none
answers "what does this do, what is it set to, who set it, and when will a
change land" in one place.

## Decision

1. **A file editor with a schema.** The settings page (`settings` action,
   `prefix-S` by default) lists every scalar setting with its effective
   value, its shipped default, the layer that set it, its description, and
   when a change applies. Committing an edit sets or removes exactly one
   dotted key in the **user's own** `config.toml` through
   `phux_config::settings::write_edit`: a `toml_edit` document mutation
   that keeps comments, order, and formatting, validated by
   `phux config check` and a full parse before the file is replaced
   atomically. Removing a key is how "reset to default" is spelled; the
   base layer shows through.

2. **Only explicit edits write.** Runtime toggles (`toggle-sidebar`,
   `toggle-zoom`, mouse opt-out) still never touch the file. The page
   writes when the user acts on a row on the page, and nowhere else.

3. **Apply is the existing reload.** A successful write asks the driver
   for the same atomic `reload-config` swap; keys a reload does not cover
   are labelled with when they land (next attach, next server start), so
   the page never implies an effect it cannot produce.

4. **The catalogue is pinned to the schema.** `phux_config::settings::CATALOG`
   carries one row per scalar key with its kind, bounds, summary, detail,
   and applies-when; a test walks the serialized schema and fails when a
   field lacks a row or a row names no field. Composite keys (widget lists,
   binding tables, hook and registry arrays) are composition, not knobs,
   and stay out.

5. **Theme slots come from the renderer.** `[theme]` is a free-form map in
   the schema; the slot vocabulary lives in `phux_tui::render::Theme`. The
   TUI contributes its slots to the page as rows of kind `Color`, and their
   test pins the list to the struct's fields.

6. **Layers are read-only from the page.** A key set by an `extends`
   layer shows that layer as its origin; resetting it is refused with the
   layer's path, because removing the key from the user's file could not
   change what the layer says. Overriding it in the user's file is allowed,
   as it always was.

7. **No `set-option` verb.** ADR-0023's rejection of an imperative CLI
   knob stands. The library half the page uses would make one trivial to
   add; this ADR does not add it.

## Why

The file stays the source of truth because the page can only produce
files a hand would: a sparse overlay, comments intact, one key per edit,
with the base layer still live underneath. `phux config show --layers`,
`git diff` on a dotfiles checkout, and a colleague reading the file all see
exactly what the page did. That is the property ADR-0023 protected, and a
settings page that re-serialized the typed config would have destroyed it
on the first save.

Validating with `phux config check` before writing turns the page into a
guided editor rather than a way to break the file from inside the TUI: a
chord that does not parse, a byte cap exceeded, an unknown enum variant is
refused with the same message the CLI gives, and the file on disk is the
one that was there before.

## Tradeoffs

- **A second consumer of the schema's vocabulary.** The catalogue's
  detail texts paraphrase the schema and `default.toml` comments; the pin
  test catches a missing row, not stale prose. Prose drift is a review
  concern, as it already is for the generated reference.
- **Symlinked configs are followed.** `write_edit` canonicalizes the path
  so a dotfiles symlink keeps pointing at its target and the target's mode
  survives. The binary's registry writer refuses symlinks; the two differ
  on purpose and the page documents its choice.
- **Composite settings stay in the file.** Widget lists, binding tables,
  hooks, and plugin registries are not editable from the page. The page
  shows where the file is and points at `phux config check`.

## Alternatives

**Runtime-only knobs (no write).** A page that changed the running client
and forgot on detach. Rejected: it is the imperative model ADR-0023
declined, minus even the persistence, and it teaches users that the page
and the file disagree.

**Re-serialize the typed config on save.** Simplest writer. Rejected: it
flattens a hand-commented file into a generated one and materializes every
default into the user's file, which is the scaffold anti-pattern ADR-0023
exists to prevent.

**Watch the file and skip the reload step.** Rejected for the reason
docs/consumers/tui.md section 4.3 already records: a saved-mid-edit file
would take effect before the user asked. The page reuses the explicit
reload instead.
