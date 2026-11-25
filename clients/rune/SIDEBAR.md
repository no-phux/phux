# Sidebar navigation

The sidebar shows every session and terminal on the server.
See [Phux Rune](./README.md#what-it-does) for the rest of the app.

## States and attention

Each terminal shows what it is doing: a declared agent's name and state
(working pulses, blocked or asking needs you, done waits until you look),
a running command, a long command that finished or failed while you were
elsewhere, and a bell.

Quiet, unboxed session summaries keep the state mark, colour and count.
⌘J jumps to what needs you; ⌘B folds the sidebar.

## Sessions and moving work

Every session has a separate header with its resource count, a separate
disclosure target and a **×** close action on hover, including empty sessions.
Closing asks before ending the named session and its running work; it does
not first switch away from the session you are using. Click a header to open
it, double-click its name to rename it, or drag its name to reorder.

Drag a terminal or document row onto another session's header or resource
rows to move it there; an empty or collapsed session accepts drops too.
Dragging a tab onto a session moves its members together. Moves stay on the
same machine and take effect after the server accepts them; dropping outside
the session list or cancelling leaves them alone.

## Disclosure and the machine header

Disclosure is independent of selection, so navigating does not unfold
sessions you have tucked away. Sessions, child panes and other machines
use one leading disclosure gutter. Left and Right fold and unfold the
focused session or parent pane. A parent's `+N` counts its child panes.

The machine picker, search and Inbox share a readable header row below
native window controls; without leading native controls they retain the
single-row layout. Machine identity stays in its labelled dot rather than
a window-wide colour strip.
