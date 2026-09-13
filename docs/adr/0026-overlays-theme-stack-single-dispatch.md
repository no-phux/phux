---
audience: contributors
stability: stable
last-reviewed: 2026-05-30
---

# 0026 — Overlays: one theme, a real stack, and a single dispatch path

**TL;DR.** Chrome and overlays share one `Theme`; overlays compose on a
real `Vec` stack behind the `RenderOverlay` trait; the command palette is
just another `RenderOverlay`; and both keybindings and overlay selections
commit a `ResolvedAction` that flows through the single `run_action()`.
There is no second execution path for "commands."

Status: Accepted
Date: 2026-05-30

## Context

The TUI grew several chrome surfaces — a status bar, a help modal, a
rename prompt, and now a command palette and a `<leader> w` window picker.
Three forces converged. First, overlay colors were once scattered
`Color::Cyan` literals per overlay. Second, overlays were modeled as a
single `Option<Box<dyn RenderOverlay>>`, which can't express "palette on
top of help." Third, and most important: a command palette invites a
second way to run actions — a parallel dispatcher keyed by command name —
which would drift from the keybinding path and double every bug.

## Decision

1. **One theme.** A single `Theme` value (named `Color` slots) is owned by
   the attach driver and snapshotted into every overlay at construction.
   Chrome and overlays resolve all color through it; `[theme]` config
   overrides layer onto the defaults.
2. **A real stack.** `OverlayState` holds a `Vec<Box<dyn RenderOverlay>>`.
   The top captures input; rendering walks bottom-up so overlays compose.
   A single active overlay is the one-element case.
3. **The palette is just an overlay.** The command palette and the window
   picker are both the same reusable `SelectList: RenderOverlay`
   primitive — a themed `Modal` + a query line + a filtered list — built
   from different item sources.
4. **One dispatch path.** Every overlay selection returns
   `OverlayCommand::Commit(ResolvedAction)`. The dispatcher feeds that
   action into `run_action()` — the exact function a resolved keybind
   feeds. Opening the palette and the picker are themselves
   `run_action()` arms (`command-palette`, `window-picker`).
5. **No registry drift.** The dispatcher owns `ACTION_NAMES`, the single
   source of truth for the handled action set. The palette's presentation
   registry is checked against it by a unit test, so a `run_action` arm
   added without a registry row (or vice versa) fails CI.

## Why

Routing palette selections through `run_action()` means a palette command
and its keybinding are provably the same behavior — there is one code path
to test and one place a bug can live. The alternative (a command-name
dispatcher next to the action dispatcher) would silently diverge the first
time someone fixed one and not the other. The `ResolvedAction` type
already crosses the keybind boundary, so reusing it costs nothing and the
palette inherits parameterized actions for free.

A real stack (not `Option`) makes "palette over help" a data property, not
a special case, and keeps each overlay ignorant of what's beneath it.

One theme makes a restyle a single edit and lets the snapshot tests pin
deliberate visual defaults rather than incidental literals.

## Tradeoffs

- The palette can only offer actions `run_action()` already handles. A
  "command" that isn't an action needs an action arm first — by design,
  since that arm is the only execution path.
- `ACTION_NAMES` plus the registry plus the `run_action` arm is a
  three-touch change to add an action. The drift test makes the omission
  loud, but it is still three edits, not one.
- Overlays snapshot the theme and config at construction, so a live config
  reload doesn't restyle an open overlay; dismiss and reopen picks it up.
  Accepted to keep `Box<dyn RenderOverlay>` `'static`.

## Alternatives

**Command-name dispatcher for the palette.** Rejected: it is the second
execution path this ADR exists to forbid; it would drift from keybindings.

**Trait object per command (a `Command` trait the palette invokes
directly).** Rejected: it bypasses `run_action()`'s effect model
(`set_metadata` broadcast, focus moves, parked spawns) that the keybind
path already centralizes; the palette would have to re-derive all of it.

**Keep `Option<Box<dyn RenderOverlay>>` and special-case stacking.**
Rejected: stacking is a real product need (palette over help) and a `Vec`
expresses it without per-call-site branches.
