# End state: the desktop and phone app

What "done" looks like for Rune's chrome. Every visual change aims here, and
[VISUAL_REVIEW](./VISUAL_REVIEW.md) is how a change proves it moved closer.
The bar is the calm, native feel of the best Mac terminal apps: when you
update, nothing looks generated, nothing jumps, and nothing needs explaining.

Judge against a live build (`cargo run -- --socket … --snap`), never only a
headless gallery frame. Gallery frames skip traffic lights, hover, and font
rendering on a real display.

## The test

Open the app, make a split, open the palette, open Settings, fold the
sidebar, close a session. A person who has used a polished Mac app finds
nothing to point at: no misaligned edge, no odd colour, no stutter, no word
that reads like a log line.

## Window

- One surface. With the sidebar folded and a single pane, the terminal runs
  to the window edge, and the traffic lights and sidebar toggle float over
  it on the terminal's own ground.
- The titlebar row holds, left to right: lights, sidebar toggle, then the
  session's title. Everything in that row shares one centre line. With the
  sidebar folded, the session's tabs take the title's place, each led by
  the same tile its sidebar row has.
- The terminal's text starts on the same left edge as the title above it.
- Splits are cards on the window ground. The focused one is raised with a
  stronger edge, with no extra colour, and the rest stay fully legible.

## Sidebar

- Rows are pills: one radius, one height, one inset from both edges.
- The selected row is a filled pill; hover is a lighter fill; there are no
  borders on unselected rows.
- One list of work: each row is a tab (or a terminal no tab holds), one
  line high, led by its tile and named after its task.
- The tile is the agent's colour and initial, or the prompt on an outline
  for a plain terminal; a badge on its corner carries the state's glyph in
  its tone, and only a state worth telling has one.
- Group headers are small, dim and medium weight, aligned to the row text.
  Their close button waits under the pointer.
- Search is an icon beside the inbox; the list starts under the top edge.

## Popovers, menus and the palette

- One radius family: the inner row radius is the popover's radius minus its
  padding, so corners nest.
- A filter field sits at the top as a filled capsule; the selected row is a
  filled pill; actions sit below a hairline with their shortcut right-aligned
  in the dim colour.
- Width fits the longest title and its shortcut, never truncating a word.

## Settings

- Grouped cards on the window ground: no outline, one radius, one inner
  padding.
- Section headers sit outside their card, bold, aligned to card content.
- Row separators are inset to the text, never full-bleed.
- Every control in a column ends on the same right edge.
- Destructive actions are tinted text on a faint tint, not a solid red block.

## Motion

- Everything that appears is caused by input, and it moves on a token spring:
  drawers slide, popovers scale from their anchor, toasts rise and fade.
- Related things move on one spring, so no edge overshoots its neighbour.
- Nothing lands more than one frame late, and Reduce Motion lands at once.

## Phone

- Targets are at least 44 points, with the reachable actions at the bottom.
- The Inbox, the sessions sheet and Settings share the desktop's card,
  row and radius rules at touch size.

## Coming from another app

`phux migrate` reads tmux, zellij, Ghostty, herdr and Rex configs and hands
the result to your agent ([migrate](../../docs/config.md#coming-from-another-multiplexer)). Onboarding
offers it on the first run, so "take me from X to phux" is one step.
