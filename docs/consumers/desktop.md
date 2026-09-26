---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-09-23
---

# Desktop

**TL;DR.** The accepted desktop contract is terminal-first: organize work with
projects, folders, and worktrees; reveal agents where they run; open independent
views of the same terminal across panes and windows. Closing a view preserves
execution. Apple-silicon macOS is the first target. This page specifies the
product; the native desktop is not yet a verified shipping implementation.

<!-- impl-status: partial; probe: GridFrame -->
> **Status: partial.** The shared runtime and immutable grid publication exist.
> The desktop interactions below are accepted requirements, not claims of
> implemented UI. Their release evidence is tracked in the final Status table.

The architecture choice is [ADR-0139](../adr/0139-solid-desktop-over-native-runtime-views.md).
Implementation kickoff is authorized. [Cockpit](./cockpit.md) remains the
separate Native SDK experiment; this is not a rename or migration of its UI.
The first release requires independent same-terminal views. Linux follows as
tracked work; Intel macOS is not a first-release requirement.

## Start with a terminal

On first launch, offer a local terminal immediately and an **Open Folder**
action alongside recent projects. Creating an account, defining a project, or
launching an agent is not a prerequisite. An existing daemon is reused through
the runtime; startup failure names the failed step and offers a concrete retry
or diagnostic action. A loading state cannot masquerade as an empty inventory.

The primary surface is the terminal. A collapsible project navigator, tabs,
split panes, a command palette, and an optional resource inspector organize it.
Menus, palette entries, context menus, and shortcuts invoke one command surface
with consistent labels, availability, and target identity.

## Projects, folders, and worktrees

These are desktop organization terms, not new server resource kinds:

| Term | Meaning |
|---|---|
| Project | A named, user-owned grouping of folder references and terminal placements; it can contain more than one folder or host. |
| Folder | A directory qualified by its host/endpoint. It supplies a default working directory for new terminals. A path on one host never resolves on another. |
| Repository | Git identity discovered for a folder when available; a plain folder is equally usable. |
| Worktree | A Git checkout associated with a repository and its own folder path; it is a place to work, not an execution session. |
| Window | A native application window showing project navigation and tabs. |
| Tab | A named presentation containing a split tree of placements. |
| Placement | A stable location in that tree referencing one runtime view. |
| Terminal | The daemon-owned resource and its running process. |
| View | An independent presentation of that terminal, with its own scroll, selection, and search state. |

**Open Folder** reuses an existing matching host-qualified folder or creates a
project with that folder. Users can rename the project and add other folders.
Worktree discovery groups related checkouts without merging their paths or
running shells. Opening a worktree selects its folder; starting a terminal
uses that folder as its requested working directory. Changing directories in
a running shell updates observed CWD but does not silently move its placement
to a different project. Manual organization remains stable.

Remote folders remain visible when offline, with their host and unavailable
state. Removing a project or folder reference does not delete disk contents or
terminate its terminals. Existing terminals can be assigned to a project
without changing their server session or restarting their process. Git
worktree creation/removal is not implied by listing and opening checkouts.

## Tabs, splits, windows, and independent views

Create, rename, reorder, split, resize, and move placements using keyboard or
pointer. Dragging a placement between tabs or windows preserves its view and
process. Minimum-size layouts remain operable; cancellation restores the
previous layout and focus. A pending spawn belongs to its original destination:
closing that destination before the reply cannot insert it into whichever pane
happens to be focused later.

**Reveal Existing** finds an existing placement and focuses its window.
**Open Another View** creates a new view of the same terminal in a chosen
split, tab, or window. Both are available in the first release. Scrolling,
selecting, or searching in one view leaves the others unchanged; all show the
same process output. Focus and input belong to the selected view, subject to
the server's input authority.

The focused writable view controls the desktop's desired PTY size. Other views
display the same authoritative grid, cropping when smaller and leaving unused
space when larger. They do not independently rewrap live application output.
The inspector identifies the controlling view, authoritative dimensions, and
read-only/observer state. Server size policy and other clients can constrain
the result. Merely opening an observer view does not resize the application.

## Close, terminate, and recover

| Action or failure | Required result |
|---|---|
| Close pane or tab | Remove those placements and release their views; leave the processes running. |
| Close window or quit | Save local presentation and detach; daemon-owned work continues. |
| Close one of two views | Preserve the sibling view, its state, and the shared attachment. |
| Terminate Terminal | Explicitly request server termination; all views reflect the terminal's closure. |
| App crash or UI update | Reattach to the same resources if their daemon incarnation still exists. |
| Transport interruption | Show reconnecting/stale state; runtime recovery owns retries. |
| Daemon crash/restart | Mark old resources missing; processes and volatile scrollback are lost. |

Termination names the target and uses server capabilities and approvals. It
does not hide behind a generic close button. [Daemon durability limits](../adr/0130-on-disk-pty-journal-is-not-built.md)
also apply to desktop restore: a layout snapshot is not a process checkpoint.
Never silently bind an old placement to a reused numeric ID on a new daemon.
Offer an explicit new terminal after loss, using a remembered folder only when
the user chooses to create it; do not replay old commands automatically.

Restore windows, tabs, split ratios, project associations, selected placement,
and view preferences from a versioned local snapshot. Remap missing monitors
into usable bounds. Offline and missing terminals keep understandable
placeholders. A corrupt or newer snapshot produces an actionable recovery
state rather than deletion of the original file. Live selection/search anchors
are generation-bound and cannot be treated as durable document positions.

## Terminal interaction

Native input covers physical keys, committed text, non-US layouts, IME
composition, mouse-reporting modes, focus reporting, and bracketed paste.
Application shortcuts arbitrate before terminal input; composing or committing
text cannot send a second copy through the physical-key path. Selection,
copy, search, and hyperlinks use the engine's document model. A view can
return to live output without changing a sibling's viewport.

Input availability reflects attachment, current identity, input readiness,
and role/lease authority. Queuing a key locally is not proof that the server
received it. Acknowledged actions distinguish **Delivered**, **Refused**, and
**Unknown**. Unknown delivery blocks further unsafe action until fresh
authoritative output has actually been presented; it never triggers blind
replay by the UI. An offscreen acquired frame is not that evidence.

Required text fidelity includes wide and combining characters, fallback fonts,
dense colors, cursor shapes, decorations, alternate-screen applications,
scrollback, resize, and clipping. Graphics support requires a separate renderer
audit: the engine's Kitty feature alone does not prove image placement. The
[verification contract](../architecture/desktop-verification.md) owns the
fidelity evidence and any explicit unsupported-case disposition.

## Agents appear where the work is

A terminal remains usable as a terminal when an agent starts. Its agent badge
and an unobtrusive detail affordance become visible from existing emitted
state. The first agent therefore reveals useful context without mandatory
onboarding or replacing the terminal with a dashboard. Project navigation and
an attention view are complementary ways to find the same work.

The inspector shows available AgentSession children, lifecycle and attention,
bounded recent events, questions, and server-held approvals. Emitted lifecycle
and detector fallback remain distinguishable; an unknown vocabulary value is
not an error or invented success. Truncation, sequence gaps, removed parents,
and stale approvals are visible. Attention notifications are deduplicated,
preference-controlled, and take the user to the relevant resource without
typing into it. Approval buttons appear only with the necessary server
capability and authority. No Objective/Run scheduler or speculative coordinator
is part of this desktop contract.

## Connections, settings, and native behavior

Local and registered remote hosts can be open concurrently. Host labels and
resource identity remain qualified throughout navigation and diagnostics.
Reuse registry credentials and the shared dialer; show authentication refusal,
expired credentials, unsupported server versions, and offline inventory as
different states. Observer and writable transitions follow server authority.

Settings show effective value, default, source, and whether an edit applies
live or at next start. Shared keys use the existing configuration catalogue
and comment-preserving writer; UI-only preferences have a desktop-owned
schema. Validate before atomic write, detect external edits, support theme and
font preview, and explain keymap conflicts. Invalid configuration leaves a
recoverable app rather than a blank window.

Native menus, Dock/reopen, dialogs, file drops, clipboard, links, notifications,
display scale, appearance, reduced motion, and high contrast are part of the
product. Keyboard focus and screen-reader navigation cover both chrome and
terminal text. Terminal titles and output remain data; links and dropped paths
cannot become implicit command execution. Diagnostics are bounded and
redacted, with connection/resource/view identity and actionable error context.

## Status

All target rows below are governed by [ADR-0139](../adr/0139-solid-desktop-over-native-runtime-views.md).
The tracking IDs name remaining work, not delivered features.

| Target | Current evidence and gap | Tracked work |
|---|---|---|
| Native Solid desktop | Shared runtime exists; matched host, binding, painter, input, and feasibility evidence remain required. | phux-d4x9.1–.6, phux-d4x9.20 |
| Terminal-first projects and workspace | Accepted ontology and interaction contract; desktop shell/layout/restore not release-verified. | phux-d4x9.7–.9 |
| Independent same-terminal views | Current runtime presentation is terminal-keyed; first-release runtime and UI proof required. | phux-d4x9.17, phux-d4x9.18 |
| Connections, settings, and agents | Existing substrate capabilities; complete desktop projection and failure journeys required. | phux-d4x9.10–.12 |
| Native accessibility and delivery safety | Required journeys and teardown/input proofs are not yet qualified. | phux-d4x9.13, phux-d4x9.14 |
| Apple-silicon release | Native regression gates and signed-package preparation remain required. | phux-d4x9.15, phux-d4x9.16 |
| Linux | Subsequent platform qualification; no support claim from framework compatibility alone. | phux-d4x9.19 |

## Where to go next

- [Desktop architecture](../architecture/desktop.md) owns runtime and binding seams.
- [Desktop verification](../architecture/desktop-verification.md) owns acceptance evidence.
- [Contributor setup](../SETUP.md) owns environment provisioning.
