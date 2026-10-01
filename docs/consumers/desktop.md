---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-09-28
---

# Desktop

**TL;DR.** This is the accepted desktop product contract, not a verified
shipping implementation. It specifies terminal work organized by projects,
folders, and worktrees, agent state shown where agents run, and independent
views across panes and windows. Closing a view preserves execution.
Apple-silicon macOS is the first target.

<!-- impl-status: partial; probe: GridFrame -->
> **Status: partial.** The Solid shell runs daily on Apple silicon: tabs,
> split trees, independent views, the command palette, agent attention,
> themes and restore are implemented. Release evidence (accessibility,
> packaging, native CI) is still open; the final Status table tracks each gap.

The architecture choice is [ADR-0139](../adr/0139-solid-desktop-over-native-runtime-views.md).
[Cockpit](./cockpit.md) remains a separate client. The first release requires
independent same-terminal views; Linux follows, Intel macOS is not required.

## Start with a terminal

First launch offers a local terminal, **Open Folder**, and recent projects;
no account, project, or agent is required. It reuses an existing daemon.
Startup failures name the failed step and offer a retry; loading must not
appear as an empty inventory. A project navigator, tabs, splits, command
palette, and optional inspector organize the terminal through one command
surface.

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

An agent's emitted state adds a badge and details to its terminal. The
inspector shows AgentSession children, lifecycle, attention, bounded recent
events, questions, and server-held approvals. It distinguishes emitted state
from detector fallback and exposes gaps and stale approvals. Notifications
are deduplicated and navigate without typing; approval buttons appear only
with server authority.

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
| Native Solid desktop | Matched host, painter and input run the shell; pane-fit geometry is native. Feasibility and presentation gates stay the acceptance record. | phux-d4x9.1–.6, phux-d4x9.20 |
| Terminal-first projects and workspace | Tabs, split trees (drag, zoom, directional focus), restore by server identity, Open Folder. Projects/worktree grouping is not built. | phux-d4x9.7–.9 |
| Independent same-terminal views | Open Another View, sole-or-last-focused size owner, per-view find. Regression coverage still required. | phux-d4x9.17, phux-d4x9.18 |
| Connections, settings, and agents | One local socket per window; `Phux.app` attaches the `default` session through the installed CLI; settings and Ghostty-config import (font, palette, keybinds, quick terminal); agent badges, urgency list, toasts. Remote hosts and approvals are not built. | phux-d4x9.10–.12 |
| Native accessibility and delivery safety | Required journeys and teardown/input proofs are not yet qualified. | phux-d4x9.13, phux-d4x9.14 |
| Apple-silicon release | `just desktop-install-app` builds an ad-hoc-signed local bundle. Native regression gates, notarization and updates remain required. | phux-d4x9.15, phux-d4x9.16 |
| Linux | Subsequent platform qualification; no support claim from framework compatibility alone. | phux-d4x9.19 |

## Where to go next

- [Desktop architecture](../architecture/desktop.md) owns runtime and binding seams.
- [Desktop verification](../architecture/desktop-verification.md) owns acceptance evidence.
- [Contributor setup](../SETUP.md) owns environment provisioning.
