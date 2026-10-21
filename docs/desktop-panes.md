# Agent panes

In the desktop app, a pane an agent drives sits slightly behind your own
panes, and its tab, pane controls, and sidebar rows carry a mark for the
agent running there. That lets you tell at a glance which panes are yours
and which an agent is working in. Focus a pane, or point at it, to bring it
forward.

The terminal inside never moves or resizes. See the
[desktop overview](./clients.md#desktop) for the rest of the app.

## Which panes are agent panes

A pane is an agent pane while phux knows an agent runs in it: one its
integration reported, one you declared with `phux agent set`, one phux
detected on screen, or an [ACP session](./acp.md).

What counts is which panes an agent is in, not who opened them. A shell you
open and then start an agent in is an agent pane. A plain shell an agent
opened, with no agent running in it, is yours. The app can't tell which
client created a pane, so it doesn't try.

The mark is for your eyes only. It doesn't change what anyone is allowed to
do in the pane.

## How depth looks

- An agent pane draws in from its edges, its shadow lowers, a soft shade
  sinks under its top edge, and its content dims a little. The agent's mark
  shows in its top-right corner.
- Focusing or pointing at the pane brings it forward, and the pane controls
  name the agent instead.
- A tab whose panes are all agent panes is shorter and fainter until you
  select or hover it. A tab with a pane that needs you never fades.
- The pane's padding absorbs the inset, so the terminal keeps its exact size
  and depth never resizes a program.
- The plain terminal style keeps only the dimming. With animations off, or
  the system's reduce motion setting on, depth changes without animating.

## Agent marks

Titlebar tabs, sidebar tab rows, terminal rows, and the pane controls start
with a glyph tinted for the agent, with the agent's state as a small dot on
its corner. Claude Code, Codex, OpenCode, Pi, Oh My Pi, Grok, Amp, Cursor
Agent, Gemini, Goose, and Aider each have their own glyph and tint. An ACP
agent whose name ends in `-acp` gets the same mark as the agent it runs.
Any other agent gets a robot glyph in a muted color.

None of these glyphs is a vendor logo.
