# Tabs and previews

## Placement and controls

Choose **Top**, **Bottom** or **Sidebar** in **Settings > Layout & notifications > Tab placement**,
or **View** in the main menu. The choice is saved with your display
preferences. Side tabs live in a fixed-height, separately scrollable area at the
foot of the sidebar. Switching sessions never inserts tabs into the session tree;
hiding the sidebar returns them to the titlebar,
and Quick Terminal keeps compact titlebar tabs. Bottom tabs sit below your work,
including in Plain Terminal mode. Plain Terminal has no sidebar, so side tabs
stand in a rail of their own beside your work; a plain terminal still hides a
lone tab, and its tabs keep the desk's shape.
If side panels leave too little room for usable bottom tabs, they temporarily
return to the titlebar; widening the window restores your Bottom preference.
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

Peeking slides the full-size workspace down and clips its bottom: terminal
dimensions never change, so programs do not reflow. Miniatures use the native
GPU painter (Metal on macOS) over read-only publications, not new terminal
attachments, and never send input or resize proposals. Background output stays
live while peeking, and thumbnail scenes are released when the shelf closes.

See the [desktop usage guide](USAGE.md#using-it) for the other tab controls.
