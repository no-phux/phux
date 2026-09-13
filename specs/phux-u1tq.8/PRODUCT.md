---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-09-12
---

# TUI daily-driver navigation and federation visibility

**TL;DR.** Make the reference TUI's existing control-plane views visible and
directly usable from its persistent chrome. Sessions across hosts, agents,
commands, Settings, Help, and copy guidance should be reachable without prior
shortcut knowledge while preserving keyboard access and narrow-terminal use.

## Summary

The reference TUI already has rich session, host, agent, command, settings, and
copy surfaces. This pass makes those surfaces read as one coherent product:
persistent chrome exposes the important destinations, every visible affordance
works with a pointer, and first-use guidance teaches the same vocabulary.

Tracking: Beads `phux-u1tq.8`. Figma: none provided; the shipped TUI visual
system and its existing rendered snapshots are the design reference.

## Behavior

1. The default status bar visibly teaches direct routes to Sessions, Commands,
   Settings, Help, and copy mode. Each route shows its default prefix
   continuation and uses a human label rather than an internal action name.
   When a shipped action has a compatibility alias, discovery surfaces show
   its documented primary chord.
2. Every destination shown in the default status hint strip is clickable. A
   click invokes the same action as its keyboard binding, including opening the
   host-grouped Sessions view, and clicks on separators or unused cells do
   nothing.
3. When the bar narrows, it removes complete destinations from the right. It
   never clips a label into a misleading partial action, and Sessions remains
   the final visible route because it is the main way to locate work across
   hosts.
4. The sidebar's Agents and Sessions headings open their full views. Individual
   rows retain their existing direct destinations, and overflow rows continue
   to open the same full views.
5. Settings appears beside Commands in the sidebar's persistent footer actions
   when enough width and height exist. Each label has its own click target.
   Sharing the row preserves maximum space for Agents and Sessions; small
   terminals give their space to management content.
6. Right-clicking session chrome exposes Sessions & hosts, Agent fleet,
   Settings, Commands & Help, and the existing session actions. Menu actions show
   their active binding when one exists and dispatch identically to keyboard
   and status/sidebar entry points.
7. The Sessions view is explicitly titled "Sessions & hosts". When host
   inventory is available, every satellite remains visible whether reachable,
   empty, or unreachable. Reachable hosts show their session count; empty hosts
   say they have no sessions; unreachable hosts show the supplied diagnostic.
8. The host-grouped Sessions view refreshes through its existing live inventory
   path. A fresh inventory replaces stale host status without closing the view,
   clearing the query, or silently changing the selected destination.
9. First-use guidance names Sessions & hosts and Settings, and explains both
   phux copy mode and the host terminal's Shift-drag selection escape hatch.
10. Titles and labels use consistent product capitalization: Settings,
    Commands & Help, Agent fleet, Windows, and Sessions & hosts. This changes
    presentation only; configured action names and keybindings remain stable.
11. Existing terminal interaction remains intact. Pane input, inner-program
    mouse reporting, native Shift-drag selection, copy mode, paste, divider
    dragging, selection highlighting, and settings file round-trips keep their
    current semantics.
12. All new pointer targets derive from the cells or sidebar rows actually
    painted. Responsive clipping, sidebar docking, error strips, and stale
    frames cannot leave an invisible or displaced action target.
