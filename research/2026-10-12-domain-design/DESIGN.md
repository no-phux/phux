---
audience: humans, contributors, agents
stability: scratch
last-reviewed: 2026-10-12
---

# Resource → Session → Workspace

**TL;DR.** Resources are things. Sessions group them. Workspaces link sessions across machines. Humans and agents act on all of it as attributed actors. Every client uses the same core model.

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

## Four rules

1. **Close a square → work keeps running.** Kill is a separate, explicit action.
2. **Empty a named session → it stays.** Quick sessions are ephemeral, by choice, not by accident.
3. **Delete a workspace → sessions stay.** It owns links, not their work.
4. **An agent acts → it is attributed.** Agents act through grants; a human can always take the keyboard back.

## Who owns what?

| Layer | Owns |
|---|---|
| Shared core model | Identity, membership, lifecycle rules. |
| Owning server | Records, execution, access enforcement, attribution. |
| Clients | Views, layout, focus. |

One resource has one session and one owner. Sessions may appear in several workspaces. Names and addresses are not identity. Offline does not mean dead. Persistent records do not promise processes survive reboot.

**Proposed design, not shipped behavior.**

[Open the visual and leave notes](DESIGN.html) · [Domain model](FULL-DESIGN.md) · [Contracts](CONTRACTS.md) · [Build plan](PLAN.md) · [Migration](MIGRATION.md) · [Browser kind](BROWSER-KIND.md)
