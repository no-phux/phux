# not-rex

Reference pass against Superlogical Rex (Mitchell Hashimoto / almonk), applied to the desktop client and the Zig cockpit. Branch is for pickup; nothing here is merged.

## What Rex actually does

Watched the Aug 28 2026 basic-functionality demo and the Sep 14 CLI demo. Chrome rules, not features:

- One window tab strip. Tabs are sessions/programs (`btop`, `htop`, a path). No second bar inside a pane.
- A split is a 1px seam on the terminal background. No rounded card, no gutter, no per-pane title, no per-pane border.
- Focus is the cursor and, at most, a hairline. Unfocused panes are not boxed.
- Motion does not reflow the grid. Tab peek pushes the surface; it does not resize it. Copy uses a shader wipe, not a layout change.
- Status (cwd, clock) sits in the window corner or the shell's own statusline, not in a pane header.

## Sources

- Mitchell Hashimoto, basic demo (speed, splits, tabs): https://x.com/mitchellh/status/2093451043661316217
- Mitchell Hashimoto, CLI / splits / events: https://x.com/mitchellh/status/2099622049325232505
- Mitchell Hashimoto, tab peek (no reflow): https://x.com/mitchellh/status/2087537750182666290
- almonk, copy shader: https://x.com/almonk/status/2107549916855959976
- YouTube mirror of the CLI demo: https://www.youtube.com/watch?v=9fcfDF8SBnc
- Public testing note: https://www.superlogical.com/updates/public-testing-beginning
- Docs: https://www.superlogical.com/rex/docs

Videos were not committed. They are on X; pull the posts if you want the frames.

## What this branch changes

Desktop (`clients/desktop`):

- Split panes no longer get a 28px title bar, rounded 6px card, or 1px box.
- Focus in a split is an inset 1px accent. A lone pane stays full-bleed.
- Split / zoom / close move to a hover cluster in the corner, so the bar is gone but the actions are not.
- Predicted PTY size no longer subtracts 2 rows for that header.
- Divider hit target stays 5px; the painted seam is 1px.

Zig cockpit (`clients/cockpit`):

- `split_divider_width` is 1, not the card gutter.
- `pane_chrome_inset` is 0. Grid, PTY, and hit-testing share the pane rect.
- Painter fills panes square, paints the seam as a line, and strokes only the focused pane. No rounded card, no per-pane border.

Not in this pass: tab peek, copy shader, mission-control. Those are motion, not the border complaint.
