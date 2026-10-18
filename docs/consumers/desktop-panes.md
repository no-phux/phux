---
audience: humans, consumers, agents
stability: evolving
last-reviewed: 2026-10-18
---

# Desktop agent panes

**TL;DR.** In the desktop, a pane an agent drives sits slightly behind your
own panes, and tabs, pane controls and sidebar rows carry a mark for the
agent harness running there. Focus or the pointer brings the pane forward.
The terminal inside never moves or resizes.

This page extends the [desktop contract](./desktop.md). The classification
and depth seams are in
[desktop architecture](../architecture/desktop.md#pane-identity).

## Which panes are agent panes

A pane is an agent pane while its terminal carries an agent badge
(`phux.agent/v1`, from a harness integration, `phux agent set`, or the
detector) or has an AgentSession bound to it (an ACP launch or an attached
stream). The desktop does not learn which client created a terminal, so it
cannot tell which panes an agent opened. Origin means which panes an agent
drives. A shell you open and then start an agent in counts as an agent pane.
A plain shell an agent spawned and that runs no agent counts as yours.

## How depth reads

- The card draws in from its edges, its shadow lowers, a soft well sinks
  under its top edge, and its content dims a little. The harness mark shows
  in its top-right corner.
- Focusing the pane or pointing at it brings it forward, where the controls
  capsule names the harness instead.
- A tab whose panes are all agent panes is shorter and fainter until it is
  selected or hovered. A tab that needs you never recedes.
- The pane padding absorbs the inset. The terminal grid keeps its exact size,
  so depth never resizes a program.
- The plain terminal skin keeps only the dimming. Turning animations off, or
  the system's reduced motion setting, moves depth without animation.

## Harness marks

Titlebar tabs, sidebar tab rows, terminal rows and the pane controls lead with
a generic glyph tinted per harness, with the agent's state as a small dot on
its corner. Each harness has its own glyph and tint: Claude Code, Codex,
OpenCode, Pi, Oh My Pi, Grok, Amp, Cursor Agent, Gemini, Goose and Aider.
ACP provider slugs ending in `-acp` map to the same harness. Any other agent
gets a bot glyph in muted ink. None of these glyphs is a vendor logo.
