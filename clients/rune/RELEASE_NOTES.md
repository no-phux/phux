The first alpha of **Phux Rune**, a phux desktop client built on
[Rune](https://github.com/anomalyco/rune). It runs beside the GPUI desktop as a
candidate replacement, on the same server and client runtime.

- A sidebar of every session and terminal; click to switch. New terminal
  (⌘T) and new session (⌘N).
- The terminal paints the shared draw list: colours, styles, underlines,
  block and box glyphs, wide characters, cursor, selection; cells snap to
  device pixels and the terminal follows the window's size.
- Keyboard input through phux's key encoder, bracketed paste, copy,
  drag/word/line selection, history scrolling, text zoom.

Alpha: no IME or dead keys, splits, mouse reporting, settings or images yet.
The macOS app is ad-hoc signed and not notarized (Control-click → Open the
first time). It needs the phux CLI and never installs or upgrades it.

Verify an archive with its `.sha256` file. See
[clients/rune/README.md](https://github.com/no-phux/phux/blob/main/clients/rune/README.md).
