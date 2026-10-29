**Phux Rune 0.1.0-alpha.4**: find, links, and history you can find your way back from.

- **Find in history** (⌘F): incremental search over the focused pane's
  loaded history, newest match first. ↩ / ⌘G walk back, ⇧↩ / ⌘⇧G forward,
  Aa for case, ⌘E searches for the selection. A miss turns the bar red.
- **Links**: hold ⌘ over a URL or an OSC 8 hyperlink to underline it and see
  its target in the status line; ⌘-click opens it (Ctrl-click on Linux).
  URLs soft-wrapped across rows work. Only `http`, `https` and `mailto`
  open, so output can't launch arbitrary URL handlers.
- **Back to live**: scrolled into history, a pane shows a position rail and
  a pill that counts new lines arriving below; click it to return.
- **Remembers** text size and the folded sidebar between launches.
- **Folded sidebar** now gets a proper titlebar showing the focused terminal,
  instead of a tall empty band.

Signed, notarized and stapled like alpha.3. Alpha: one window, no settings
window or images yet. It needs the phux CLI and never installs or upgrades
it. Verify an archive with its `.sha256` file. See
[clients/rune/README.md](https://github.com/no-phux/phux/blob/main/clients/rune/README.md).
