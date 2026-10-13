---
audience: humans, contributors, agents
stability: scratch
last-reviewed: 2026-10-12
---

# Resource → Session → Workspace

**TL;DR.** One world of work for people and bots: resources are things, sessions group them, workspaces link sessions across machines. Inbox catches unorganized work. Every client uses the same core model.

## The whole picture

```text
┌─ WORKSPACE: Ship the product ───────────────────────────────┐
│                                                          │
│  ┌─ SESSION: Frontend ─────┐   ┌─ SESSION: Backend ──────┐ │
│  │ Laptop                 │   │ Mac mini               │ │
│  │                        │   │                        │ │
│  │ ┌────────┐ ┌────────┐  │   │ ┌────────┐ ┌────────┐  │ │
│  │ │Terminal│ │Browser │  │   │ │Terminal│ │ Other  │  │ │
│  │ └────────┘ └────────┘  │   │ └────────┘ └────────┘  │ │
│  └────────────────────────┘   └────────────────────────┘ │
│                  Sessions linked, not copied.             │
└──────────────────────────────────────────────────────────┘
```

**A square is a view of a resource.** The same resource can appear on desktop and phone without duplicating its execution. Arrangement is presentation, not ownership.

**Inbox is the starting place, not another container.** Existing groups stay intact. Ungrouped work lands in the owning machine's Inbox session; the Inbox view brings it together across machines. Move by drag, keyboard, or bulk selection—with undo. Organization never restarts execution.

## Four rules

1. **Close a square → work keeps running.** Kill is a separate, explicit action.
2. **Empty a named session → it stays.** Quick sessions are ephemeral, by choice, not by accident.
3. **Delete a workspace → sessions stay.** It owns links, not their work.
4. **People and bots use the same world.** Bots can operate thousands of resources; people see groups, outcomes, and exceptions. Actions remain attributed and authorized.

## Who owns what?

| Layer | Owns |
|---|---|
| Shared core model | Identity, membership, lifecycle rules. |
| Owning server | Records, execution, access enforcement, attribution. |
| Clients | Views, layout, focus. |

One resource has one session and one owner. Sessions may appear in several workspaces. Names and addresses are not identity. Offline does not mean dead. Persistent records do not promise processes survive reboot.

**Proposed design, not shipped behavior.**

[Open the visual and leave notes](DESIGN.html) · [Domain model](FULL-DESIGN.md) · [Contracts](CONTRACTS.md) · [Build plan](PLAN.md) · [Refactor map](REFACTOR-MAP.md) · [Migration](MIGRATION.md) · [Browser kind](BROWSER-KIND.md)
