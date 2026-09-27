---
audience: agents, contributors
stability: evolving
last-reviewed: 2026-09-27
---

# Native terminal input

**TL;DR.** `input::TerminalInput` is a GPUI `EntityInputHandler` backed by the
existing FFI client registry and an actual runtime `ViewId`;
`terminal/input_host.rs` wires it into the painter. Its manual fixture is
test-only: shipping builds must not enable `input-fixture`.

## Wiring contract

- One `Entity<TerminalInput>` and one `FocusHandle` per mounted element, never
  shared across views or windows; recreate on binding change, drop on destroy.
  Native focus, not the JS `focused` prop, authorizes input. Blur cancels the
  platform composition as well as local state.
- After a successful paint, `presented` receives the painted frame and the
  painter's geometry and cursor bounds. Never re-measure fonts or use JS cell
  sizes. A different connection epoch or replica identity is permanently
  rejected on a bound entity, so an old IME callback cannot re-arm; viewport
  changes reject actions until matching geometry is painted.
- Key events: `Consumed` and admission errors stop propagation; `Platform`
  reaches native text/menu handling. Printable key-down only saves metadata and
  the platform commit sends the text; named/control/Option-as-Alt keys send on
  key-down. JS never sends a second text event.
- Pointer and scroll use the painter's hitbox and GPUI capture so releases and
  drags reach the originating view. Initial hits in grid padding are rejected.
- Copy/Paste bind to `copy_selection` and `paste_text`. A paste correlation is
  not delivery confirmation; outcomes stay with the sole FFI event owner.

## Admission and semantics

Every action resolves `phux_client_ffi::napi::initialize().client(handle)`;
there is no second registry, listener, socket or event drain. Under one
`Client::with_control` closure admission checks view membership, publication
and replica identity, connection epoch, painted dimensions, viewer role and
runtime readiness/delivery fencing, then queues the operation.

Local selection, copy and scroll need current identity and focus but not input
ownership. Shift at press selects locally instead of reporting the mouse; a
gesture keeps its routing until release. Shift-wheel uses
`ControlPlane::scroll_view`. Fractional wheel deltas accumulate; application
reports are capped at 100 ticks per event.

Clipboard copy uses the runtime's bounded selection API (1 MiB cap) and never
copies a truncated string. The editable text model holds only pending IME text
(4096 UTF-8 bytes); terminal output is never advertised as a document. UTF-16
ranges snap to complete graphemes. Candidate bounds are the painted cursor
rectangle.

## Limits

GPUI `Keystroke` exposes layout-resolved names, not scan codes, modifier sides
or Num Lock, so layout-independent physical keys and keypad fidelity are **not
qualified**. Option-as-Alt defaults off (`set_option_as_alt`). Manual acceptance
remains for real OS IMEs (CJK, dead keys), candidate placement under scale
changes, focus loss mid-composition, menus, cross-app clipboard, VoiceOver and
hardware layouts. Selection autoscroll, search UI and link activation are not
implemented here.

## Reproduce the fixture

The fixture drives GPUIX's offscreen Metal renderer, the shared `DesktopClient`,
a real PTY-backed server and a raw-input recorder. Mark/commit callbacks are
injected into the real `EntityInputHandler`; they are not OS IME sessions, and
`input-fixture` simulates window activation because offscreen windows are
inactive.

1. In `src/lib.rs`, temporarily add
   `#[cfg(feature = "input-fixture")] #[path = "../../tests/native/input_fixture.rs"] mod input_fixture;`
   and call `input_fixture::install(registry)` in the extension installer.
2. Temporarily add `phux-server-testkit`, `tempfile`, `tokio` (`process`,
   `time`) and `portable-pty` dev-dependencies plus an `[[example]]` named
   `input_server` at `../tests/native/input_server.rs`.
3. With the Apple environment helper, the repo Rust pin and a private target,
   build the host and `input_server` with `--features input-fixture`, copy the
   host dylib to a `.node` file, and run `input_server /absolute/input.node`.
4. Restore the temporary wiring.
