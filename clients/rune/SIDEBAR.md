# Sidebar navigation

The sidebar shows the server's sessions and screen-addressable resources:
terminals, accessibility trees, pixel surfaces and supported documents.
See [Phux Rune](./README.md#what-it-does) for the rest of the app.

## States and attention

Each terminal shows what it is doing: a declared agent's name and state
(working, blocked or asking needs you, done waits until you look),
a running command, a long command that finished or failed while you were
elsewhere, and a bell.

Declared program/config glyphs take precedence over bundled Claude, Codex and
OpenCode marks. Other tools and resource kinds have neutral glyphs, never a
guessed provider or live URL. State keeps its shape, colour and spoken word.
Quiet, unboxed session summaries keep the attention count.
⌘J jumps to what needs you; ⌘B folds the sidebar.

## Sessions and moving work

Multi-row and empty sessions have a quiet header. A one-resource session shares
one compact row with its resource: the separately labelled session name in its context line is still its
select, rename and reorder handle. Every local session keeps a **×** close
action on hover, including empty sessions.
Closing asks before ending the named session and its running work; it does
not first switch away from the session you are using. Click a header to open
it, double-click its name to rename it, or drag its name to reorder.

Drag a terminal or document row onto another session's header or resource
rows to move it there; an empty session accepts drops too.
Dragging a tab onto a session moves its members together. Moves stay on the
same machine and take effect after the server accepts them; dropping outside
the session list or cancelling leaves them alone.

## Inline groups and machines

Local resources are always visible, without arrows or an empty disclosure
gutter. Previous window-only session/child folding is no longer used.
Terminal ancestry does not make a split: descendants remain ordinary rows.
Only an actual multi-member tab has a shallow layered identity and an inline
member list. Its lead identity stays stable while its main press returns to
the remembered member. Click a member directly, or use Left/Right on the
focused row to move between members. The loudest member's attention remains
visible even when it is not the lead.

Plain singles stay flat. Browser producers remain separate tree/surface mirrors;
they are not guessed into a browser group. Unknown resource kinds keep a neutral
identity fallback but do not become unchecked terminal navigation targets.

Other machines remain lazy: click their labelled name/count to show or hide a
glance. Opening the glance does not switch machines or invent a new connection.

The machine picker, search and Inbox share a readable header row below
native window controls; without leading native controls they retain the
single-row layout. Machine identity stays in its labelled dot rather than
a window-wide colour strip.
