---
audience: agents, contributors
stability: evolving
last-reviewed: 2026-10-13
---

# Native terminal input

**TL;DR.** `input::TerminalInput` is a GPUI `EntityInputHandler` holding
`Weak<Client>` and an actual runtime `ViewId`. The terminal view wires it.
There is no string registry and no second listener.

## Wiring contract

- One `Entity<TerminalInput>` per mounted view, never shared across views or
  windows. The view owns the `FocusHandle` and passes it in. A rejected
  presentation (connection epoch or replica identity change) replaces the
  input entity so an old IME callback cannot re-arm; the focus handle stays.
  Native focus, not a JSON prop, authorizes input. Blur cancels the platform
  composition as well as local state.
- After a successful paint, `presented_at` receives the painted frame, the
  epoch the painter already proved, and the painter's geometry and cursor
  bounds. Never re-measure fonts. A different connection epoch or replica
  identity is permanently rejected on that entity. Viewport changes reject
  actions until matching geometry is painted.
- Key events: `Consumed` and admission errors stop propagation; `Platform`
  reaches native text and menu handling. Command/platform chords are
  `Platform` and are not sent to the PTY. Non-Command `app_chords` return
  before input so the window can run them. Text-producing and dead keys defer
  key-down until the platform commits text. Control characters, including
  Enter's newline and Tab's tab character on macOS, send key atoms on key-down.
  Control/Option-as-Alt keys also send on key-down.
- Pointer and scroll use the painter's hitbox and GPUI capture so releases and
  drags reach the originating view. Initial hits in grid padding are rejected.
- Copy/Paste bind to `copy_selection` and `paste_text`. `Terminal::request`
  runs Copy, Paste, CopyText and Open once on this entity, without a keyboard
  focus check, and does not retain the action for a later render. A clipboard
  paste is the user's own action, so it is trusted. One that fits a single
  `APPLY_INPUT` is acknowledged; a larger one, or one for a satellite pane,
  goes as one atomic `INPUT_PASTE` and is never split. Outcomes stay with the
  connection's event drain.

## Admission and semantics

Every action upgrades `Weak<Client>`. A dropped client is `StaleHandle` and
does not join the runtime on the UI thread. Under one `Client::with_control`
closure admission checks view membership, publication and replica identity,
connection epoch, painted dimensions, viewer role and runtime readiness, then
queues the operation.

Local selection, copy and scroll need current identity and focus but not input
ownership. Shift at press selects locally instead of reporting the mouse; a
gesture keeps its routing until release. Shift-wheel uses
`ControlPlane::scroll_view`. Fractional wheel deltas accumulate; application
reports are capped at 100 ticks per event.

Clipboard copy uses the runtime's bounded selection API (1 MiB cap) and never
copies a truncated string. The editable text model holds only pending IME text
(4096 UTF-8 bytes). UTF-16 ranges snap to complete graphemes. Candidate bounds
are the painted cursor rectangle.

## Limits

GPUI `Keystroke` exposes layout-resolved names, not scan codes, modifier sides
or Num Lock, so layout-independent physical keys and keypad fidelity are **not
qualified**. Option-as-Alt defaults off. Manual acceptance remains for real OS
IMEs (CJK, dead keys), candidate placement under scale changes, focus loss
mid-composition, menus, cross-app clipboard, VoiceOver and hardware layouts.
Selection autoscroll and search UI are not implemented here. Link activation
is the view's Command-click.
