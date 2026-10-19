# Native agents

**Native Agents** (`native-agents`, ⌃⌘A, the titlebar toggle, View menu,
and Settings > Appearance) snaps every agent pane to its conversation, or
back to its terminal. This window preference (`nativeAgents` in the saved
display object) also opens conversations published by later agents. Each
pane can still switch back until the preference changes again; `conversation`
toggles only the focused pane. Without this explicit preference, passive
stream discovery never switches faces or keyboard focus.

An agent with no AgentSession offers its harness's documented plugin install
command to copy (Claude Code, Grok, OMP, Pi, OpenCode), or **New Agent…** for
harnesses without a hook plugin. Nothing is installed by the app.

Prompts appear in accent bubbles and replies as markdown under the harness
mark. Thinking and tool calls fold to one line; live thinking and failed
calls start open, and your fold choice holds for that entry. A working turn
shows a shimmer line, a blocked turn a "needs you" line. Stop and Send share
the composer card. These use GPUI Kit components, not vendored chat code.

**New ACP Agent** lists configured ACP-capable integrations and their host
prerequisites. Launch creates one server-owned host and one headless provider
session; desktop and mobile observe and control that same AgentSession.
Closing the desktop detaches without stopping the ACP worker. A graceful phux
handoff preserves the provider session, activation, and structured transcript.
The desktop keeps that SDK owner's local draft, pending receipts, and question
edits when the handoff recreates layout placements. Controls stay disabled until
the fresh connection confirms the same parent, AgentSession, and activation.

The conversation labels **ACP** and **Attached TUI** are deliberate.
ACP Send, Stop, and provider dialogs use typed operations against the
exact activation, current turn, and question. Permission choices, confirmations,
selection, text/editor input, multi-question answers, and supported scalar JSON
forms never become terminal keys or automatic approvals. The first human
response across viewers reserves the request. Queued/submitted is not provider
consumption; an uncertain reply is shown without automatically sending again.
Attached TUI conversations keep their existing terminal-backed controls and
**Open Terminal** handoff for terminal-only approvals.

Native profiles cover OMP, Pi, Claude Code, OpenCode, Codex, Amp, Grok, Gemini
CLI, Cursor Agent, and Goose; Aider is terminal-only. Operators install and
authenticate the native ACP executable or maintained adapter explicitly.
The picker explains unavailable prerequisites and never installs or accepts
terms for you. For remote sessions, launch on the executing host; the desktop
does not create a local SDK to impersonate remote work. Setup and exact receipt
semantics are in [the ACP guide](../../docs/acp.md).

Project panes declared with `integration:` can prefer their configured native
backend. Arbitrary `command:` or `exec:` panes remain terminals, even when their
argv resembles an agent CLI. SDK archive recipes resume known provider sessions
with fresh destination bindings, not archived controller secrets or source IDs.
