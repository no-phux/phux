# Native agents

**Native Agents** (`native-agents`, ⌃⌘A, the titlebar toggle, View menu,
and Settings > Appearance) snaps every agent pane to its conversation, or
back to its terminal. This window preference (`nativeAgents` in the saved
display object) also opens conversations published by later agents. Each
pane can still switch back until the preference changes again; `conversation`
toggles only the focused pane. Without this explicit preference, passive
stream discovery never switches faces or keyboard focus.

An agent with no AgentSession offers, in its pane's controls capsule, its
harness's documented plugin install command to copy (**Set up conversation**:
Claude Code, Grok, OMP, Pi, OpenCode), or **Launch natively** for harnesses
without a hook plugin. Nothing is installed by the app.

Prompts appear in quiet bubbles and replies own a readable, compact thread.
The provider and current state share one header; its terminal icon opens the
underlying terminal. Thinking and tool calls fold to one line; live thinking and failed
calls start open, and your fold choice holds for that entry. A working turn
shows a shimmer line, a blocked turn a "needs you" line. Send stays at the
composer's trailing edge; Stop appears when interruption is available.
Shortcut help is contextual rather than a permanent footer. Questions,
read-only states, missing transcript data and delivery failures stay visible.
These use GPUI Kit components, not vendored chat code.

**New Agent…** lists ACP-capable integrations and executable prerequisites
from the connected host, including a built-in OpenCode profile. A configured
profile takes precedence over the built-in one. The picker shows the focused
terminal's host, persistent session and absolute folder; it never inherits
the desktop process's folder. Start creates one server-owned host and one headless provider
session; desktop and mobile observe and control that same AgentSession.
Closing the desktop detaches without stopping the ACP worker. A graceful phux
handoff preserves the provider session, activation, and structured transcript.
The desktop keeps that SDK owner's local draft, pending receipts, and question
edits when the handoff recreates layout placements. Controls stay disabled until
the fresh connection confirms the same parent, AgentSession, and activation.

The header's details distinguish **ACP** from **Attached TUI** controls without
putting protocol badges in the working thread.
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
terms for you. Remote discovery and launch use the window's existing authenticated
phux connection: no SSH command string, separate OpenCode listener, credential
copy, or local agent impersonating remote work. The host must advertise
`NATIVE_AGENT_LAUNCH`; older hosts explain that they need updating rather than
falling back to terminal typing. Setup and exact receipt semantics are in
[the ACP guide](../../docs/acp.md).

## OpenCode onboarding

1. Open the local or remote host in Desktop, then focus a terminal in the
   checkout you want OpenCode to work on.
2. Choose **New Agent…** from the terminal menu or command palette (search
   `OpenCode`). The picker shows the destination host, session and folder.
3. If OpenCode is missing on that host, **Installation guide** opens the official
   instructions. Install it there, then **Refresh**; phux does not install it.
4. Existing OpenCode provider credentials are reused. If you need to authenticate,
   **Sign in on this host…** opens `opencode auth login` as an explicit argv in a
   new terminal on that same host and in that folder. Complete OpenCode's own
   login, then return to **New Agent…**. phux never asks for or copies credentials.
   An absolute executable override uses that same executable for login; a custom
   wrapper without a known login action uses its own documented authentication.
5. **Start agent** runs `opencode acp` on that host and opens its structured
   conversation locally. Model, agent, skills, instructions, plugins and project
   configuration are OpenCode's, resolved on the executing host.

Availability means the executable was found, not that a model or provider is
authenticated. Creation is not provider readiness; setup failures remain in the
conversation and notifications. A slow launch is reported without a second launch.
If a receipt is lost, inspect the original host's panes before creating another.
Prompts and permission answers are never automatically replayed after disconnect.

This is OpenCode's official ACP mode, which starts a private OpenCode backend,
not an attachment to its shared HTTP background service. It removes the remote
full-screen TUI, but does not promise one shared OpenCode process across all
conversations. No replacement agent engine or OpenCode network port is involved.
Typing plain `opencode` in a shell still launches its normal TUI; phux does not
silently rewrite shell commands. **Native Agents** can also show an Attached TUI
conversation, but that view does not turn the existing TUI into a headless session.

Project panes declared with `integration:` can prefer their configured native
backend. Arbitrary `command:` or `exec:` panes remain terminals, even when their
argv resembles an agent CLI. SDK archive recipes resume known provider sessions
with fresh destination bindings, not archived controller secrets or source IDs.
