**Phux Rune 0.1.0-alpha.8**: a window that recovers, renders faster, and speaks plainly.

- **Connection recovery.** A dropped connection comes back by itself, and the
  window says where it stands: starting, connecting, no sessions, or a server
  that needs updating, each with what to do next.
- **Faster rendering.** A scrolled row keeps what it already built. Scrolling
  rebuilds 128 rows where it rebuilt 2,944, and a stream beside an idle split
  128 where it rebuilt 10,112. Box drawing, shades and Powerline glyphs now
  line up across cells.
- **One command table.** The shortcuts, the palette and the native menus run
  the same commands by the same rules.
- **⇧⌘W** closes the tab.
- **Agent attention** shows what needs you without drawing continuously, so an
  idle window stays idle.
- **Readable themes and keyboard Settings.** Every theme keeps its text legible,
  and Settings works without a mouse.
- **Copy Diagnostics** puts the facts a bug report needs on the clipboard.
- **Install it** with `curl -fsSL https://phux.sh/install | sh -s -- --rune`.

From alpha.6 this arrives by itself; earlier alphas need one manual download.
Signed, notarized and stapled. Alpha: one window on one machine. See
[clients/rune/README.md](https://github.com/no-phux/phux/blob/main/clients/rune/README.md).
