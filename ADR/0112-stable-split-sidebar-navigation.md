---
audience: contributors, agents
stability: stable
last-reviewed: 2026-09-11
---

# 0112 — Sidebar navigation stays put while status changes

**TL;DR.** The TUI sidebar has two fixed areas, Agents and Sessions.
Lifecycle changes update badges in place. Sessions show their serving host;
the current session expands its windows. This supersedes ADR-0089's
attention-first layout and ordering, retaining its client-side peer data model.

Status: Accepted
Date: 2026-09-11

## Context

[ADR-0089](./0089-three-zone-attention-sidebar.md) deliberately let an empty
attention queue disappear and sorted rows by urgency and last change. In daily
use this moves both navigation targets and section headers while the user is
working. The sidebar becomes difficult to learn spatially and a changing agent
can displace the session the user was about to click. Host-qualified session
inventory now exists (ADR-0107), but the strip hides that context.

## Decision

1. **Two fixed panels.** After reserving the footer, half the sidebar body
   belongs to Agents and the remainder to Sessions. Population never changes
   this split. Both areas keep headers and quiet empty states. Only an explicit
   viewport resize changes their allocation. Tiny viewports degrade within
   their bounds, prioritizing Sessions when only one body row is available.
2. **Stable agent order.** Sessions use stable session-id order; agents retain
   their session's window/leaf order. State, attention, timestamps and review
   flags never participate in sorting. Agent creation, removal and explicit
   layout changes may change membership; lifecycle transitions do not.
3. **Sessions include the current session.** Session name and host are separate
   rows, with current-session windows nested below. The tab strip remains the
   complete window-navigation surface. Each panel has bounded overflow: Agents
   opens the fleet dashboard, Sessions opens the session picker.
4. **Host identity comes from the server.** Local-to-server sessions use the
   existing whoami record's hostname, with `this server` as an honest fallback.
   Satellite sessions use their host inventory's routing alias, and their
   clicks carry both host and session name. A local session containing a
   satellite pane remains a session on the serving server. An unreachable host
   remains visible as an inert placeholder.
5. **Project once, paint incrementally.** Peer metadata and inventory replies
   dirty the sidebar model. The burst drain rebuilds it once and schedules
   changed chrome without clearing pane contents. Peer layout broadcasts use
   the same agent-watch reconciliation as GET replies.
   Workspace membership, not mirror-slot allocation, identifies local agent
   broadcasts. Early question flags survive until the persisted layout resolves
   ownership; the server need not repeat a coalesced question.

## Consequences

- Calm periods leave useful breathing room instead of moving Sessions upward.
  The user can learn where each area and ongoing agent lives.
- Urgency remains visible through glyphs and color, without overriding spatial
  navigation. The explicit fleet dashboard may keep its task-oriented ranking.
- A full panel can hide a newly blocked agent below overflow. That trade-off is
  deliberate: attention shortcuts and the fleet remain available; the sidebar
  is a stable navigation surface.
- Review lifetime remains a separate client-state defect (phux-deya). A session
  switch can still reset review badges, but it cannot reorder sidebar rows.
- No wire or server state is added. Host inventory is a snapshot refreshed by
  existing lifecycle/picker sweeps, not a claim of a new live federation feed.

## Validation

- Empty, working, blocked and completed fleets leave both headers on the same
  rows at a fixed viewport; identical projections emit no bytes.
- Focus/review and lifecycle transitions preserve agent target order.
- Session labels identify the serving hostname and satellite alias; identical
  names on different hosts retain distinct click actions.
- GET and broadcast peer changes reach a quiet visible sidebar with the fleet
  closed; a broadcast introducing a leaf requests its metadata and watch.
- Short/narrow viewports remain bounded, overflow stays reachable, and real
  terminal captures verify top tabs and split sidebar together.
