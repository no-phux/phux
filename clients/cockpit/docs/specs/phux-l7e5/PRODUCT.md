---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-09-09
---

# Cockpit shared Phux workspace

**TL;DR.** Cockpit displays the sessions, shared windows, and split terminals
that Phux already owns. Changes from another client become discoverable without
reconnecting. Each client keeps its own selected window, focused terminal, and
native window placement. Returning to a session restores that session's view
without displaying stale panes from a different session as usable terminals.

## Summary

Implements Beads `phux-l7e5`, explicitly requested by the user on 2026-09-09.
The existing terminal chrome and navigation controls remain the interaction
surface; no new visual mock was supplied.

## Behavior

1. Connecting to a session adopts its shared windows as tabs and its split
   leaves as panes. A fresh Cockpit configuration sees the existing composition.
2. A session without shared composition exposes all its existing terminals as
   individual tabs, rather than selecting and exposing only the first terminal.
3. Sessions and terminals created, renamed, or removed by another client become
   discoverable while Cockpit remains attached. On a responsive local server,
   idle discovery starts at least once per second; it also runs on explicit
   navigation refresh. A slow server never causes overlapping refresh buildup.
4. When shared composition exists, terminals outside it remain available in
   navigation. Listing a terminal does not itself attach or resize that terminal.
5. Selecting a session shows only that session's workspace. A -> B -> A restores
   A's local selection when the target still exists. Selecting an exact terminal
   from another session switches sessions and selects that terminal.
6. Shared topology changes preserve local selection and focus when those targets
   survive. Another client's focus, window index, or recently active session
   never steals Cockpit's focus. Deleted selections use a surviving nearby target.
7. Renaming and reordering a shared window preserve its identity. Its identity
   does not depend on a current tab index, display name, or native window handle.
8. New Terminal, Split, and New Window create durable Phux terminals. Their
   composition changes are visible to other clients using the same shared layout.
   Native presentation-window placement remains local to Cockpit.
9. Moving a tab or resizing a split updates shared composition. Closing a
   Cockpit presentation does not silently kill the durable server process.
10. A creation result applies to its captured session and destination. Changing
    focus or session while a request is pending cannot redirect it. An unknown
    creation outcome is never retried automatically.
11. Concurrent topology writes follow the existing shared layout's
    last-write-wins behavior. Cockpit reconciles the server's winning value;
    it does not indefinitely retain an unconfirmed local arrangement.
12. Reconnecting to the same server/session adopts current shared composition.
    A stale Cockpit layout file cannot overwrite newer shared topology.
    Reused numeric identities after server replacement do not regain old input
    authority merely because their numbers match.
13. While disconnected, last-good terminal presentation may remain visible as
    unavailable. It accepts no input. Session switching never makes another
    session's frozen pane look live.
14. Invalid, unsupported, or over-capacity shared composition is reported as
    unavailable for adoption. Cockpit keeps its last-good view without silently
    truncating a tree or writing a reduced layout back to Phux.
15. Switching between two individually supported sessions does not charge the
    replacement session against the old session's retained replica capacity.
16. Direct local scratch terminals remain ephemeral and separate from a
    Phux-authoritative workspace. Satellite identities retain their host
    qualification; colliding local numbers never imply shared ownership.
17. Existing keyboard, pointer, clipboard, terminal rendering, search, and
     accessibility behavior continue to operate on the selected live pane.
18. This pre-1.0 cutover uses shared layout schema v3 with required stable window
    IDs. Previous schemas are explicitly unsupported; existing metadata is not
    silently rewritten. Initial attachment waits for confirmed metadata presence
    or absence. An authoritative empty composition remains empty.
