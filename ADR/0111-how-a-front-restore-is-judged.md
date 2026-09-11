---
audience: contributors
stability: stable
last-reviewed: 2026-09-11
---

# 0111 — How a front restore is judged

**TL;DR.** Amends ADR-0110. A front record whose host's connection fails
once before it is shown waits for the backoff redial, a lister, and is
judged when that connection lists; a second failure drops it. This Mac's
first projection at launch leaves a restored peer's selected tab in front.

Status: Accepted
Date: 2026-09-11

## Context

[ADR-0110](./0110-a-showing-peer-is-re-shown-at-launch-only-in-front.md)
re-shows at launch the one remembered host whose tab was the front
window's selected tab, once its own list still carries the session and the
front window has a measured size. Two cases were left open.

A host whose first connection failed dropped its record (ADR-0110 decision
5), so a host that was merely slow to accept its first dial came back
listing for the whole launch.

The order of the first projections was not settled. If This Mac's first
projection landed after the restored peer's and took the selection, the
peer would be hidden, return to listing, and the restore would be lost.

Rules A and B of ADR-0110 hold throughout: a coordinator that is not
displaying never attaches and never holds a viewport, and input, edits and
metadata writes go only to the coordinator that minted the ref.

## Decision

1. **One retry.** A pending front record survives one failed connection
   of its host, whether it fails before the host lists or after it lists
   but before a frame measured the front window. What that connection
   listed is forgotten. The backoff redial, which like every automatic
   redial only lists, is judged when it lists, by the same checks as the
   first connection: the session still listed, not empty, and a measured
   front window. A second failure before the record is shown drops it.
   Any choice the user makes meanwhile cancels it, as ADR-0110 decision 5
   does for the first connection.
2. **The launch race.** This Mac's first projection at launch carries no
   remembered selection, because a Phux launch reads no topology state
   file. Landing after the restored peer's first projection, it places its
   tabs beside the peer's and leaves the peer's tab selected, so the peer
   stays shown. A choice that lands with it, such as a navigation to one
   of This Mac's terminals, takes the tab. The peer is then hidden and
   returns to listing as any hidden peer does: its tabs leave, and only
   its own connection restarts, as a lister that sends no ATTACH.

## Why

Retrying once covers a host that was still starting, or whose network was
not up yet, at launch. It costs nothing under rule A: the redial is a
lister's, so nothing attaches until its list judges the record, exactly as
on the first connection. Retrying only once bounds how late a restore can
take the front window. The first redial comes about a second after the
failure; a host that returns later comes back listing rather than pulling
the front window away from whatever the user is doing by then.

The race needs no new rule. A peer's first projection takes the selection,
and a later projection takes it only with a choice behind it. Rule A is
kept by the existing `settlePeers`, which returns any peer whose tabs are
not on screen to listing.

## Tradeoffs

- A host that fails twice stays listing for that launch, even if it comes
  back a minute later.
- A restore after a failure appears a second or more after launch.
- Between the failure and the redial the record stays front in the
  `.remote` file, so a quit then keeps it for the next launch.

## Alternatives

**Retry on every redial for the whole launch.** A host that returns a
minute later would take the front window from whatever the user is doing,
which a restore should never do.

**Retry on a faster timer of its own.** A second timer per slot for one
case, where the backoff redial already exists and already only lists.

**Make This Mac's projection yield the selection to a restored peer.** A
special case in the shared workspace for a state that already resolves
correctly without one.
