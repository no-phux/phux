---
audience: humans
stability: experimental
last-reviewed: 2026-10-27
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

The overview counts live agents, working agents, and those that need you.
Each card puts the harness mark and agent name first, then the terminal's real
title and exact session, machine, and terminal. Colored status pills distinguish
working, ready, review-ready, and blocked agents; urgent attention reads
**Needs you**. Status changes do not reshuffle the cards. Light themes use
contrasting label ink, and narrow windows become a scrollable single column.
There are no fabricated CPU, activity, or throughput charts.

Cards enter with a short, bounded cascade. It finishes in about half a second
and does not keep an idle fleet repainting. **Appearance > Animations** and
system **Reduce Motion** disable it. A selected agent stays selected through
cold session restore, regardless of bootstrap order. Later navigation cancels
its pending focus change without removing a preexisting open or quiet session
tab. An attach refusal is reported once and never automatically retried.
Browsing never acquires terminal views, changes
PTY sizes, sends agent input, or acknowledges attention. Turning the experiment
off removes its titlebar and palette entry; terminals keep running.

## Playground

The header's playground button swaps the cards for the same agents as fox
pieces on an isometric campus: a room per machine, Local first, with a door, a desk
per agent and a dimmed empty room for a machine that is not connected. A
working piece roams its room, a blocked one bobs at its desk, an idle one
sleeps, a fresh finish celebrates; the name and the same status word as the
card sit under each piece. Drag to pan, scroll to pan, pinch to zoom. The
camera opens on the first room that needs you. Clicking a piece opens that
exact agent, as a card does, and the keyboard path is unchanged. The map
button beside it switches the skin between office and matrix, the same two
maps the phone has; the choice is saved with this window's display
preferences. A piece
never leaves its machine's room. The scene draws about twelve frames a
second while it is open; **Appearance > Animations** off or system **Reduce
Motion** leaves every piece on a still frame. The palette button cycles the
piece's colours: theme (the default, the fox in the window theme's text
colours, light on dark themes and dark on light ones), then gold, silver,
mint, rose, aurora, ember and matrix; the choice is saved with the display
preferences. The piece is the Emblem Fox, the PHUX mark, animated from a
phux-brand export; it is recoloured only here.

See the [desktop README](README.md) for installation and development, and
[desktop usage](USAGE.md) for the rest of the app.
