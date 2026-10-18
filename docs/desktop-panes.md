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

See the [desktop overview](./clients.md#desktop) for the wider client.
The [classification and extension seam](#classification-and-extension) below
explains how this presentation is derived.

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

## Classification and extension

`clients/desktop/native/src/app/pane_identity.rs` computes one `PaneIdentity`
for a placement: what it displays (`PaneKind`) and who drives it (`PaneOrigin`).
Cards, tabs, and navigator rows consume that identity instead of inventing
separate agent checks.

`PaneKind` currently selects `Terminal` or `Conversation`, the native face of a
bound AgentSession. A new non-terminal kind would also need a placement
payload and persisted-layout representation: the layout currently names
terminals. A new renderer alone is not enough.

`PaneOrigin` is `Human` or `Agent(Harness)`, not creator provenance. These
are presentation classifications, not authorization principals;
[connection authority](./remote.md#connection-authority) is a separate grant.
Clients cannot see `Provenance.created_by`; human and agent CLI connections announce
the same `phux-cli` name, and terminal spawns record no parent. Classification
uses the published agent badge or bound AgentSession. The session's provider
outranks a badge's declared kind. A future creator field would be another
explicit input, not something to guess from a process name.

`Harness` maps built-in kinds, binary aliases, and ACP provider slugs (including
`-acp` suffixes) to a generic Lucide mark and light/dark tint. Unknown slugs use
the muted bot mark.

Depth is a skin value (`Material.depth`), not terminal geometry. `Motion::depth`
recedes an agent-driven placement only when it is unfocused, not hovered, and
not leaving. Reduced motion and disabled animations land immediately. Padding
absorbs the inset, so the terminal content box and every size proposal stay
identical. Tabs recede only when all placements are agent-driven, none needs a
person, and the tab is not selected.
