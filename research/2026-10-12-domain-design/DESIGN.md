---
audience: humans, contributors, agents
stability: scratch
last-reviewed: 2026-10-12
---

# Resource → Session → Workspace

**TL;DR.** Resources are things. Sessions group them. Workspaces link sessions across machines. Every client uses the same core model.

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

## Three rules

1. **Close a square → work keeps running.** Terminate is a separate action.
2. **Empty a session → the session stays.** Its identity is not tied to a process.
3. **Delete a workspace → sessions stay.** It owns links, not their work.

## Who owns what?

| Layer | Owns |
|---|---|
| Shared core model | Identity, membership, lifecycle rules. |
| Owning server | Records, execution, access enforcement. |
| Clients | Views, layout, focus. |

One resource has one session and one owner. Sessions may appear in several workspaces. Names and addresses are not identity. Offline does not mean dead. Persistent records do not promise processes survive reboot.

**Proposed design, not shipped behavior.**

[Open the visual and leave notes](DESIGN.html) · [Full technical design](FULL-DESIGN.md) · [Full annotated HTML](FULL-DESIGN.html)
