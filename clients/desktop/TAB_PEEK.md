# Peek tabs

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
