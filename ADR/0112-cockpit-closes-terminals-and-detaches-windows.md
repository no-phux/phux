---
audience: contributors
stability: stable
last-reviewed: 2026-09-11
---

# 0112 — Cockpit closes terminals and detaches windows

**TL;DR.** Close Pane and Close Tab end the selected work. Close Window and
Quit detach Phux-backed views without ending work or removing shared layout.
Cockpit-created sessions opt into keep-empty. Closing their last terminal
leaves an Empty session view; closing that view retains the named session.
Scratch terminals remain ephemeral. These rules apply to every command surface.

Status: Accepted
Date: 2026-09-11

## Context

The approved [everyday UX contract](../specs/phux-2jza/PRODUCT.md) behavior 19
distinguishes resource termination from client-view lifetime. The previous
Cockpit shared-window removal was layout-only, despite being exposed as Close
Tab. OS-window closure and tab closure cannot share that operation.

[ADR-0105](./0105-sessions-can-outlive-their-last-window.md) makes keep-empty an
opt-in session property; [ADR-0107](./0107-satellite-sessions-are-listed-never-adopted.md)
preserves real satellite terminal ownership even when a hub projects its leaf.
Cockpit's [emptied-window decision](../clients/cockpit/docs/DECISIONS.md)
predates keep-empty. None of these permits a client to claim detach after kill.

## Decision

| Action | Phux-backed work | Ephemeral scratch work |
|---|---|---|
| Close Pane; Cmd+W with a pane focused | End the exact terminal and its agent children; remove its shared pane. | End the terminal and remove the pane. |
| Close Tab | End the tab's terminals and remove its shared layout. | End the tab's terminals and remove the tab. |
| Close Window; macOS close button | Remove only this window's views; retain terminals and shared layout. Other windows remain attached. | End scratch terminals owned by the window. |
| Disconnect Machine | Remove this app's views/connections for that machine; retain work and saved registration. | Not applicable. |
| Quit | Detach all Phux views; retain work and layout for reopening. | End this app's scratch terminals. |
| Process exit | Mark the terminal ended; remaining panes continue. Preserve a keep-empty session after its last exit. | End the terminal; remaining panes continue. |
| End Session | End the named session and its terminals on its owning coordinator, visible to other clients. | Not applicable. |

**Keep-empty is explicit.** New Session in Cockpit opts in. Existing CLI-created
sessions keep their lifetime setting. Closing the last terminal in a keep-empty
session shows Empty session with New Tab. Cmd+W there closes the client window,
retaining the named session. An emptied window otherwise closes. The last macOS
window closing quits the app. This is the keep-empty exception to the earlier
emptied-window decision; it does not change default CLI session reaping.

**A view is not its execution authority.** Several windows showing one session
share its layout and terminal identities. Closing one window releases only its
view; the session attachment is released when no visible view needs it. One
canonical PTY geometry is derived from the existing visibility/layout policy,
never by letting independent windows alternately resize the same terminal.
Sessions A and B on one machine use independent attachment contexts, fenced by
session id and creation time. Selecting B in one window leaves A elsewhere.

**Destructive completion is authoritative.** Close Tab and End Session use
all-or-nothing resource teardown where supported. Live views are not removed on
enqueue: retain them until an acknowledged outcome or authoritative ended
publication. A refusal keeps the work visible with its reason. An unknown or
partial outcome refreshes the owning coordinator and reports per-terminal
truth; it is never blindly resubmitted on a replacement connection. Captured
actions retain resource, session, connection and invoking-view identity.

**Satellite leaves remain real terminals.** Close Pane/Tab terminates their
satellite resources through the existing relay, preserving ADR-0107. Its term
“window” means a server layout window (Cockpit tab), not an OS window. Close
Window/Quit detach those views like other Phux-backed terminals; no satellite
session id is adopted or interpreted as a hub session id.

**Cleanup is distinct from intentional termination.** Failed or abandoned spawn
cleanup remains conditional on its instance token and unattached state under
[ADR-0109](./0109-late-kills-are-conditional-on-instance-and-attachment.md).
Changing closure behavior does not authorize unconditional delayed cleanup.
Connection loss freezes the last output, refuses new input and shows
Reconnecting, rather than reporting process exit. A cold restart that lost work
does not recreate its identity from a reused numeric id.

## Why

Pane/tab closure follows ordinary terminal intent; window closure is safe for
durable work. Explicit labels and server-owned outcomes make the same action
predictable from pointer, menu, keyboard and Commands. Keeping lifetime on the
session lets CLI and GUI observers agree after the last terminal exits.

## Tradeoffs

- Close Tab now ends work rather than only hiding its layout. Tests and labels
  must cover the changed authority, including agent-child cascade.
- Detaching a view needs projection ownership independent of resource teardown.
- Each matrix row needs an independent CLI/client observer; a disappearing
  Cockpit pane is insufficient evidence that the intended resource survived or
  ended. Headless model checks do not establish native-window acceptance.

## Alternatives

**Keep layout-only Close Tab.** Rejected: it contradicts the approved action and
leaves running work hidden by an ordinary terminal-ending control.

**Kill on Close Window.** Rejected for Phux-backed work: it conflates a client
projection with the lifetime of shared, durable execution.

**Make every session keep-empty.** Rejected: it changes existing CLI policy and
server self-exit semantics. Cockpit opts in only when it creates a session.
