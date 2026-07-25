---
audience: contributors
stability: stable
last-reviewed: 2026-07-25
---

# 0058 — Right-click context menus

**TL;DR.** The right button opens an anchored context menu over client
chrome and over panes whose inner program has not asked for the mouse:
a pane menu, a window menu on a status-bar tab or sidebar row, a session
menu everywhere else. Rows commit `ResolvedAction`s through the existing
`run_action` path. A pane that tracks the mouse keeps the button.

Status: Accepted
Date: 2026-07-25

## Context

ADR-0048 gave the client its own outer-terminal mouse capture, and the
left button now has meaning everywhere: click-to-focus in a pane,
drag-to-resize on a divider, drag-to-copy over a non-tracking pane, tab
and sidebar-row selection on the chrome. The right button had none. Over
a pane it was forwarded as `INPUT_MOUSE`; over the sidebar strip or the
status-bar row it hit the `Left`-only guards in `input_dispatch` and was
consumed and dropped. Right-clicking phux did nothing, which reads as
broken rather than as unimplemented.

Every affordance the client already has is reachable by chord or by the
command palette, so what was missing is not capability but *proximity*:
the actions that apply to the thing under the pointer, at the pointer.

## Decision

1. **Three menus, one per target.** A right press on a pane opens the
   pane menu (splits, zoom, copy mode, the palette, close). On a
   status-bar window tab or a sidebar window/agent row it first runs
   `select-window` — a menu must act on what you pointed at — and then
   opens that window's menu. Anywhere else on the chrome it opens the
   session menu. A divider keeps its ADR-0048 behavior (grab-to-resize on
   the left button; everything else dropped).
2. **Rows are `ResolvedAction`s.** `crates/phux-client/src/attach/context_menu.rs`
   builds rows; the dispatcher commits them through `run_action`, the
   same path a keybinding, a palette row, and a sidebar click take. A
   lockstep test asserts every menu action is in `ACTION_NAMES`. Rows
   carry the chord bound to them, so the menu teaches its own keyboard
   equivalent.
3. **A tracking pane keeps the button.** The gate is the one
   drag-to-copy already respects: if the inner program enabled mouse
   tracking, every button is its own — vim, htop, and TUIs with their
   own right-click menus are untouched. The bindable `context-menu`
   action is the way into the pane menu for those panes, and the way in
   from the keyboard generally.
4. **Any-motion reporting is raised only while a menu is open.** The
   menu hover-tracks the row under the pointer, which needs `?1003h`;
   the driver reconciles that mode against the overlay stack each loop
   iteration exactly as it reconciles capture against focus, and drops
   back to `?1002h` on close.
5. **Menus are pinned, not centered.** The box's top-left corner sits on
   the anchor cell, flipping left/up at the edges and clamping into the
   pane content rect, so it never occludes the sidebar or the bar
   (phux-foz.14's invariant for centered modals).

## Why

Committing `ResolvedAction`s rather than bespoke click handlers is the
same choice ADR-0048's sidebar and status-bar clicks made, and it is why
this ADR is short: no new dispatch semantics exist to reason about, the
menus cannot drift from what the dispatcher does (a test fails), and a
user's rebinding shows up in the menu annotation for free.

Anchoring the corner *on* the pointer is what makes the two interaction
idioms coexist without a mode flag. The pointer rests on the border, so
the release that ends the opening click cannot land on a row: press,
drag, release picks (macOS); press, release, move, click also picks
(Windows). A menu placed one cell down-right would commit its first row
on any slightly-slow click.

Refusing the button to a tracking pane is the boundary the client already
draws for the left button. Stealing it would break the exact programs
most likely to define their own right-click behavior, and the host
terminal's own Shift-bypass is not available to us as an escape hatch —
most terminals consume Shift+click themselves and never report it.

## Tradeoffs

`?1003h` while a menu is open means one motion report per cell crossed,
each triggering an overlay repaint. That is the cost profile copy-mode
drag already accepts, bounded by how long a menu stays open, and the
mode is off the rest of the time.

The menus are a fixed row set, not config-driven. A user who wants
different rows has no knob short of a rebuild; the palette remains the
complete surface. Making them configurable is a later change that this
one does not close off — the rows are already `(label, action)` pairs.

Panes running a mouse-tracking program have no pointer route to the
menu at all. That is deliberate, and `context-menu` covers it, but it
does mean the feature is not uniformly available by pointer.

## Alternatives

**Forward right-click and put the menus behind a modifier.** Rejected:
the natural modifier (Shift) is eaten by the host terminal before it
reaches us, and any other (Alt, Ctrl) collides with what inner programs
receive.

**One menu for everything, opened anywhere.** Simpler to build, but it
defeats the point — a context menu that ignores context is a worse
command palette, and phux already has a good one.

**Reuse `SelectList` (the palette widget) as the menu.** It is centered,
carries a query line and a footer, and filters. A menu that appears in
the middle of the screen with a text box is not a menu; the shared
piece worth reusing was `Modal`, and it is.
