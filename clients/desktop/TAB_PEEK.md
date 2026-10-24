# Tabs and previews

## Placement and controls

Choose **Top**, **Bottom** or **Sidebar** in **Settings > Layout & notifications > Tab placement**,
or **View** in the main menu. The choice is saved with your display
preferences. Side tabs live in a stable-height, separately scrollable area directly
below the window header, above the session catalog. Switching sessions never inserts tabs into the session tree;
hiding the sidebar returns them to the titlebar,
and Quick Terminal keeps compact titlebar tabs. Bottom tabs sit below your work,
including in Plain Terminal mode. Plain Terminal has no sidebar, so side tabs
stand in a rail of their own beside your work; a plain terminal still hides a
lone tab, and its tabs keep the desk's shape.
If side panels leave too little room for usable bottom tabs, they temporarily
return to the titlebar; widening the window restores your Bottom preference.
With side or bottom tabs and the sidebar visible, the sidebar owns the window
header: your panes extend up to the top gutter instead of keeping an empty
tab strip. Changes, agent views, notifications, appearance and update controls
sit above the sidebar's search field. Hiding the sidebar brings them back into
a compact header, with no extra gap above the panes. Window dragging still
works in the header and, in the full-height layout, the top sheet gutter.
Selecting, closing,
double-clicking to rename, right-clicking for tab actions, and dragging to
reorder work in every layout. The command palette also offers **Tabs on Top**,
**Tabs on Bottom**, **Tabs in Sidebar**, and the three **Group Sessions** views.

## Hover context

Hover a tab to see its title, panes, current folders and agent states without
switching to it. The context card floats over your work on every tab placement;
large splits show a compact list and a “+N more” count.

## Peek tabs

Click the **selected tab** to peek at all open tabs as live native terminal
miniatures, including their split layouts and attention marks. Click a preview
to open that tab, or use **Peek Tabs** in the command palette (⇧⌘\\).
Left/Right browse; Enter opens; Escape or a click on the displaced work closes
the peek. Selecting an inactive tab still switches directly, and dragging,
modifier-selection and double-click rename keep their usual behavior.

Split tabs show a layered mosaic of their pane layout. The focused pane remains
lit on inactive tabs, and zoom dims hidden panes without removing their cells.
Harness marks keep their identity when agents need attention; the corner pip
reports state instead of replacing the mark.

Peeking slides the full-size workspace down and clips its bottom: terminal
dimensions never change, so programs do not reflow. Miniatures use the native
GPU painter (Metal on macOS) over read-only publications, not new terminal
attachments, and never send input or resize proposals. Background output stays
live while peeking, and thumbnail scenes are released when the shelf closes.

See the [desktop usage guide](USAGE.md#using-it) for the other tab controls.
