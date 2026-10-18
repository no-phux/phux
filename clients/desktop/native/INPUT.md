# Native terminal input

`input::TerminalInput` is a GPUI `EntityInputHandler` holding
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
- Pointer and scroll use `Geometry::hit`, the same device-pixel edges as
  paint. GPUI capture keeps releases and drags on the originating view.
  Initial hits in grid padding are rejected. The selection gesture and
  `INPUT_MOUSE` carry that cell in the integer pixel grid the resize reports
  as `cell_px`, so selection, copy, and a program's cursor land on the painted
  cell on both screens.
- A Command-click link is captured from the painted frame on press and opened
  on release only when the pointer is still on that same link. The address is
  the cell's OSC 8 target, or a URL written in the row, limited to the schemes
  in `input/link.rs`. It is never read from a publication newer than the
  paint. Dragging off the link, a scroll or resize, or a replaced connection
  cancels the gesture. Output that leaves the link and the viewport unchanged
  does not. A captured application-mouse release is still sent when the live
  frame is briefly missing or a different size; a replaced connection or an
  unknown-delivery fence is not.
- Copy/Paste bind to `copy_selection` and `paste_text`. `Terminal::request`
  runs Copy, Paste, CopyText and Open once on this entity, without a keyboard
  focus check, and does not retain the action for a later render. A clipboard
  paste is the user's own action, so it is trusted. One that fits a single
  `APPLY_INPUT` is acknowledged; a larger one, or one for a satellite pane,
  goes as one atomic `INPUT_PASTE` and is never split. Outcomes stay with the
  connection's event drain.
- ACP conversation controls do **not** enter this terminal input path.
  `ControlPlane::native_agent_command` targets the exact AgentSession/activation,
  current turn or current question. Kit owns its text fields and dialogs;
  dedicated native receipts distinguish queued submission from admission.
  Disconnected/stale ACP controls never fall back to a paste, Return or Escape.
  Pending launches and retained native ownership also fence raw input and file
  drops before reading files or starting uploads, even when the native view is
  temporarily unavailable. Explicit **Attached TUI** mode retains terminal input
  semantics only for terminals without a native owner.

## Admission and semantics

Every action upgrades `Weak<Client>`. A dropped client is `StaleHandle` and
does not join the runtime on the UI thread. Under one `Client::with_control`
closure admission checks view membership, publication and replica identity,
connection epoch, painted dimensions, viewer role and runtime readiness, then
queues the operation.

Local selection and copy need current identity and focus but not input
ownership. Shift at press selects locally instead of reporting the mouse; a
gesture keeps its routing until release. The wheel scrolls the view under the
pointer in its own window, focused or not, with current identity. Shift-wheel
uses `ControlPlane::scroll_view`. A local scroll keeps a continuous position
inside the scrollback: whole rows move the view through one control turn, and
the remaining fraction of a row stays on the input entity for the painter
([PAINTER.md](PAINTER.md#smooth-scroll)). A move within the current row takes
no runtime turn. Every local scroll notifies the input entity, so the
observing terminal repaints in the next frame. Whether the program reports the
wheel is asked once per replica sequence, since only output changes mouse
modes. A reporting program gets whole-line presses: fractional deltas
accumulate across events, a gesture's first event (`TouchPhase::Started`)
discards the previous remainder, and reports are capped at 100 ticks per
event. A local selection change republishes the view without a runtime wake,
so the input notifies its painter after a press, drag or release; the desk
does the same after keyboard scroll, prompt jumps, follow, select-all and
find.

Clipboard copy uses the runtime's bounded selection API (1 MiB cap) and never
copies a truncated string. The editable text model holds only pending IME text
(4096 UTF-8 bytes). UTF-16 ranges snap to complete graphemes. Candidate bounds
are the painted cursor rectangle. A wide tail uses the head cell.

## Limits

GPUI `Keystroke` exposes layout-resolved names, not scan codes, modifier sides
or Num Lock, so layout-independent physical keys and keypad fidelity are **not
qualified**. Option-as-Alt defaults off. Manual acceptance remains for real OS
IMEs (CJK, dead keys), candidate placement under scale changes, focus loss
mid-composition, menus, cross-app clipboard, VoiceOver and hardware layouts.
Selection autoscroll and search UI are not implemented here. Link activation
is the painted-frame Command-click described above.
