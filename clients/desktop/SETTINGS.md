---
audience: humans
stability: evolving
last-reviewed: 2026-10-23
---

# Settings

**⌘,**, **Phux > Settings…**, or the command palette's gear opens a separate
native window, leaving your terminals visible. Its sidebar has **Appearance**,
**Interface**, **Shortcuts**, **Terminal**, **Ghostty**, **Updates**, **Machines**,
and **Developer**.

Search Settings (or **⌘F** / **Ctrl-F**) filters categories and individual controls.
**Escape** clears a search, then closes the window; **⌘W** closes only Settings,
never a terminal. Reopening brings the existing window forward on its current page.

Changes apply and save automatically; sliders save on release. Display preferences
apply to the workspace window named in the titlebar, while update and developer
preferences retain their app-wide scope. Opening Settings from another workspace
retargets it to that window. Machine setup and confirmation dialogs open in the
originating workspace. Closing that workspace also closes its Settings window.

Individual controls are also searchable in the [command palette](PALETTE.md).
See [desktop usage](USAGE.md) for navigation and [updates](USAGE.md#updates).

## Appearance

**Interface style** offers **Classic** (solid, quiet surfaces with full navigation)
and **Glass** (native translucent material). **Glassy panes** independently lets
the desktop show through pane backgrounds; **Pane opacity** controls how much.
Turning glassy panes off remembers the chosen opacity. Classic, Plain terminal,
and **Blur behind window** off keep panes solid. Plain terminal remains a separate
minimal-chrome mode, not the Classic style.

Chrome uses bundled Inter with compact leading and consistent label columns.
**Font family** and **Font size** still change only terminal text; the searchable
font dropdown previews each face, with bundled terminal fonts pinned first.
