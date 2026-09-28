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
[Cockpit](./cockpit.md) remains a separate client. The first release requires
independent same-terminal views; Linux follows, Intel macOS is not required.

## Start with a terminal

First launch offers a local terminal immediately plus **Open Folder** and
recent projects; no account, project, or agent is a prerequisite. An existing
daemon is reused; a startup failure names the failed step and offers a retry,
and loading never masquerades as an empty inventory. The terminal is the
primary surface, organized by a project navigator, tabs, splits, a command
palette, and an optional inspector, all driven by one command surface.

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

**Open Folder** reuses a matching host-qualified folder or creates a project
for it. Worktree discovery groups checkouts without merging paths; a new
terminal starts in the selected folder, and a shell's later `cd` never moves
its placement to another project. Offline remote folders stay visible.
Removing a project or folder reference deletes nothing and terminates
nothing; assigning an existing terminal to a project never restarts it.

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

Termination names its target and goes through server capabilities and
approvals. A layout snapshot is not a process checkpoint
([ADR-0130](../adr/0130-on-disk-pty-journal-is-not-built.md)): never bind an
old placement to a reused id on a new daemon, and never replay commands.
Restore windows, tabs, ratios, projects, and view preferences from a
versioned local snapshot, with placeholders for missing terminals and a
recovery state (not deletion) for a corrupt or newer snapshot.

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

A terminal stays a terminal when an agent starts; a badge and detail
affordance appear from emitted state. The inspector shows AgentSession
children, lifecycle, attention, bounded recent events, questions, and
server-held approvals, keeping emitted state distinguishable from detector
fallback and making gaps and stale approvals visible. Notifications are
deduplicated and navigate without typing; approval buttons appear only with
server authority.

## Connections, settings, and native behavior

Local and registered remote hosts open concurrently with host-qualified
identity, through the shared registry and dialer; auth refusal, expiry,
version mismatch, and offline inventory are distinct states. Settings show
effective value, default, source, and when an edit applies, using the shared
catalogue and comment-preserving writer. Native menus, Dock, dialogs, drops,
clipboard, notifications, scale, appearance, reduced motion, high contrast,
and screen-reader navigation are in scope; titles, links, and dropped paths
never become implicit command execution.

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
