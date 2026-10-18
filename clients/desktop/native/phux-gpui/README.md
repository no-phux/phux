---
audience: consumers, contributors, agents
stability: evolving
last-reviewed: 2026-10-18
---

# phux-gpui

**TL;DR.** A phux terminal as a GPUI view. `Terminal::new(client, view,
terminal, settings, window, cx)` paints one runtime publication with native
GPUI glyphs and sends keys, mouse, IME, paste and resizes back through
`phux-client-runtime`. The phux desktop is built on it. It builds on the
published gpui-fast 0.1.9; the `patched-gpui` feature adds what phux's GPUI
patches provide.

## Use it

```rust
let client = Arc::new(Runtime::connect(Target::uds(socket), options)?);
// once the session is attached:
let view = client.create_view(&terminal_id)?;
let terminal = cx.new(|cx| Terminal::new(client, view, terminal_id, Settings::default(), window, cx));
```

`Terminal::new` borrows the runtime view identity; it does not destroy the
`ViewId` when the entity is dropped. When closing a view on a shared
`Client`, the host must call `client.destroy_view(view)`. Keep sibling views
and the shared client alive.

Wake GPUI from the runtime's `Listener`, drain `Client::take_activity` on
the UI thread, and `cx.notify()` the terminal on `Event::TerminalChanged`.
[`examples/embed.rs`](examples/embed.rs) does all of it in one file:

Use the [isolated example command](../../../../docs/embedding.md#gpui)
from the repository root after `just desktop-source`. It requires an explicit
disposable socket and non-production profile; do not use an inherited pane
socket or the installed CLI.

Give the view a bounded width and height. Set `Settings::size_owner` on the
one view that should resize the PTY. The rest of the view's contract is in
[PAINTER.md](PAINTER.md), [INPUT.md](INPUT.md) and
[PRESENTATION.md](PRESENTATION.md).

## Dependencies

The crate depends on `gpui` as `gpui-fast = "=0.1.9"` (its library is named
`gpui`) and on the phux client crates by path; nothing here is on crates.io
yet. The [embedding guide](../../../../docs/embedding.md#build-requirements)
has the one build setting libghostty needs, and the routes for apps that are
not GPUI.

## `patched-gpui`

Enable it only when your build serves gpui-fast from phux's patched sources
(`clients/desktop/toolchain`, `just desktop-source`, and the desktop's
`[patch.crates-io]`). `src/platform.rs` is the only file that knows.

| Patch | With the feature | Without it |
|---|---|---|
| 0006 bulk glyph sprites | A row's glyphs are retained atlas sprites, one paint call per row. | Glyphs paint from their shaped lines each frame. Same glyphs, more work per frame. |
| 0001 drawable presented | A delivery fence clears only after the exact drawable was presented on a visible window. | It clears when the next frame begins: the scene was submitted, not proven on screen. |
| 0005 press-and-hold | A held key repeats on macOS. | macOS shows its accent menu for a held letter. |

Patch 0003 has no API and no feature: on macOS it ties a fallback family to
the font data registered in memory. On published gpui-fast, a bundled Nerd
symbol can resolve to an installed font of the same PostScript name, or to a
missing-glyph box when none is installed.

The pixel fixtures and perf budgets run with the feature, through
`phux-desktop`.
