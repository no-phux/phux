---
audience: contributors
stability: stable
last-reviewed: 2026-09-11
---

# 0110 — A showing peer is re-shown at launch only in front

**TL;DR.** Cockpit remembers, beside each remembered remote host, the one
session that host's coordinator was showing, keyed by its coordinator and
that server's incarnation. At launch every host lists first. Only the host
whose tab was the selected tab of the front window is shown again, at the
window's real size, and only once its own list still carries that session.

Status: Accepted
Date: 2026-09-11
Superseded in part by [ADR-0111](./0111-how-a-front-restore-is-judged.md): decision 5's drop of a front record whose first connection fails.

## Context

Cockpit holds up to four Phux coordinators: the active one and up to three
peers. A peer lists sessions over GET_STATE and attaches only while one of
its tabs is the selected tab of an open window. Two rules come from past
defects. (A) A coordinator that is not displaying never attaches and never
holds a viewport, because under `window-size smallest` its viewport would
clamp every other client of that session. (B) Every ref carries its
coordinator's id, and input, edits and metadata writes go only to the
coordinator that minted the ref.

Relaunch restores which hosts are held (the `.remote` file beside the
workspace state file), and each comes back listing. A peer that was showing
a session comes back only as a list, so after every relaunch the user picks
the same remote session again. A Phux-backed launch reads no topology state
file: a coordinator's windows, tabs, splits and focus live in its own shared
workspace, and placements saved under `.phux` before coordinator ids existed
never match anything. The open question was what client-side state a peer's
layout needs, how a restored peer can attach without breaking rule A, and
what happens when its server changed in the meantime.

## Decision

1. **What is kept.** For each remembered host whose coordinator is showing a
   session (a peer that shows one, or the active coordinator when it is a
   remembered host), Cockpit keeps one record: the session id, a hash of the
   server's `HELLO_OK.server_id`, the shared window id of that
   coordinator's selected tab, and whether that tab is the selected tab of
   the front window. Nothing about terminals, splits or other tabs is kept:
   the coordinator's shared workspace remains their only source.
2. **Where.** In the `.remote` file, as a `shown=` line directly after its
   host's `target=` line. With no record the file stays the v2 list that
   earlier releases read; with a record it is written as v3. It is written
   only when its bytes change, atomically. A file naming more than one
   front keeps the first and reads the others as not front; any other
   malformed file is treated as absent, as before. No topology state file is read or
   written for a Phux coordinator.
3. **Keying.** A record belongs to the target line it follows, so its
   coordinator id is `phuxCoordinatorId(target)`. It is consulted only by
   the held coordinator with that id, and only for a session of that server
   incarnation. A bare session id never selects anything.
4. **Launch.** Every remembered host is reattached listing, as before.
   When a host's first list on this launch arrives, its record is judged:
   - A record not marked front never attaches anything. Its tab was not on
     screen in front, so its peer only lists. The record's shared window id
     is kept as a selection hint in case the user picks that session.
   - The front record shows its session through the ordinary show path,
     once the list still carries that session, not as an empty session,
     and the front window has been measured. The attach carries that window's
     real grid, not the 80 by 24 default. Its first projection selects the
     remembered tab when that tab still exists, and the first of its tabs
     otherwise, so the peer is displaying from the moment it attaches.
   At most one record is front, and a remembered host's live front tab
   outranks a loaded front not shown yet, so at most one peer attaches.
5. **Reconciling.** A record is dropped quietly, and nothing is shown or
   sent, when the session is no longer listed, the server hash differs (the
   server re-executed, so ids may name other sessions), the session is
   empty, the coordinator is no longer held, or its first connection fails
   first. Any selection the user makes before the list arrives cancels a
   pending front restore, and so does any change of the active
   coordinator (Connect to Host, Use this Mac, Disconnect). A renamed session keeps its id and incarnation
   and is restored under its new name. A record is never rewritten to
   another coordinator: dropping is the only fallback.
6. **Removal.** Disconnect removes the host's line and its record together;
   Disconnect All removes the file. A record under a host that is no
   longer remembered cannot be read.
7. **Old placements.** Placements saved before coordinator ids, under the
   old `.phux` provider id, are not migrated. A Phux launch never read them
   and still does not.

## Why

The coordinator already owns the layout. Keeping only which session was on
screen, and which of its tabs, adds nothing a server could contradict. The
client never has to merge a stale tree with a live one.

Listing first makes the restore conditional on the server as it is now: the
same incarnation, the same session, still with a window. Showing through
the ordinary pick path reuses the existing checks (the pending retarget, the
peer's own slot, one restart) and the existing rule that a shown peer's
first projection takes the selection. Attaching is safe under rule A only
when the peer displays at once, which holds only for the front window's
selected tab. Every other tab would attach for nothing.

Holding the attach until the front window has a measured size is what makes
the first viewport real. A placeholder grid would size every other client of
that session to it until the sizing pump corrected it.

## Tradeoffs

- Only one peer comes back on screen. A peer that was showing in a second
  window, or behind another coordinator's tab, comes back listing.
- A server re-exec, including a graceful upgrade, drops the record even if
  the session survived. The instance token of ADR-0109 would survive an
  upgrade, but a client can read it only from a bound spawn.
- The restore waits for the host's first list and the first frame, so a
  slow host appears a little after launch. A host whose first dial fails
  does not come back on screen at all on that launch.
- An older release reading a v3 file forgets every remembered host, which
  costs one Connect to Host after a downgrade.

## Alternatives

**Persist the full client layout per coordinator** (windows, tabs, trees)
in the topology state file. This duplicates the shared workspace and needs
a merge whenever the two disagree, and a restored tree could route a ref to
a coordinator that no longer holds it.

**Re-show every peer that was showing, as placeholder tabs** that attach
when selected. This adds a tab kind with no terminals to every surface that
assumes a tab holds terminals, for a state that is rare at quit.

**Attach every restored peer at launch and let `settlePeers` return the
hidden ones to listing.** This holds viewports on sessions nobody is looking
at, however briefly, which is exactly what rule A forbids.

**Match by session name instead of id and server.** Names are not unique
over time: another session could hold the name after a rename. Matching by
name would show a session the user never had on screen.
