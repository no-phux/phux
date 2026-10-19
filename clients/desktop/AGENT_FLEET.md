---
audience: humans
stability: experimental
last-reviewed: 2026-10-19
---

# Agent fleet

Enable **Settings > Layout & notifications > Experimental > Agent fleet**.
The choice is saved with this window's display preferences; no restart or
environment variable is needed. Open **Agent Fleet** from the titlebar's grid
button or search for it in the command palette.

The live grid shows agents across Local and registered machines, with their
session, terminal, lifecycle state, and attention. Tiles keep their positions
when states change. Click a tile, or use **arrow keys** and **Return**, to open
that exact agent in this window. **Escape**, the close button, or clicking
outside returns to your work. Refresh retries unavailable machines; offline
agents are not presented as live.

Tiles cascade into place with colored state emblems. These are not CPU or
throughput charts. The entry animation finishes in about half a second and
does not keep an idle fleet repainting. **Appearance > Animations** and system
**Reduce Motion** disable it. Browsing never acquires terminal views, changes
PTY sizes, sends agent input, or acknowledges attention. Turning the experiment
off removes its titlebar and palette entry; terminals keep running.

See the [desktop README](README.md) for installation and development, and
[desktop usage](USAGE.md) for the rest of the app.
