---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-10-14
---

# Desktop

**TL;DR.** This is the accepted desktop product contract, not a verified
shipping implementation. It specifies terminal work organized by projects,
folders, and worktrees, agent state shown where agents run, and independent
views across panes and windows. Closing a view preserves execution.
Apple-silicon macOS is the first target.

<!-- impl-status: partial; probe: GridFrame -->
> **Status: partial.** Tabs, split trees, independent views, the command
> palette, agent attention, themes, and restore are the implemented product.
> The host is now GPUI; a presented-frame qualification is still required.
> Accessibility, notarization, and Linux are open. The Status table tracks each gap.

The host seams are [desktop architecture](../architecture/desktop.md). [ADR-0165](../adr/0165-gpui-desktop-replaces-the-solid-host.md) replaces the Solid host in [ADR-0139](../adr/0139-solid-desktop-over-native-runtime-views.md).
This is the maintained native client. The Zig [Cockpit](./cockpit.md) is
archived outside main ([ADR-0169](../adr/0169-zig-cockpit-lives-in-an-archive.md)). The first release requires
independent same-terminal views; Linux follows, Intel macOS is not required.

[Install the Desktop](../INSTALL.md#desktop-app-macos)
from the first-party installer on an Apple-silicon Mac running macOS 27 or
later. It downloads a published `Phux.app`, installing the CLI first if it is
missing.

To build the same app from a checkout instead, install the CLI, set up the
desktop tools from [Contributor setup](../SETUP.md), quit any running
`Phux.app`, and run `just doctor desktop && just desktop-install-app`. That
replaces `/Applications/Phux.app` with an ad-hoc-signed development build
without updates; it never replaces the CLI. `PHUX_PROFILE`, `PHUX_SOCKET`,
and `PHUX_SESSION` override the server and session it attaches.

## Start with a terminal

First launch offers a local terminal, **Open Folder**, and recent projects;
no account, project, or agent is required. It reuses an existing daemon.
Startup failures name the failed step and offer a retry; loading must not
appear as an empty inventory. A project navigator, tabs, splits, command
palette, and optional inspector organize the terminal through one command
surface.

### Intake organization

Untargeted startup opens the owner's Intake: a real designated persistent
Session for work created without a destination. The sidebar puts that Session
and separate imported Sessions awaiting organization under **Intake sessions**,
preserving their existing grouping. **Mark session organized** in an imported
Session's menu clears only its import mark, removing it from that section
without moving, renaming, or restarting its work. The attention **Inbox**
lists agent attention alerts; it is not a Session or a move destination.
The titlebar bell opens **Inbox & notifications**, separating agent alerts
from errors and command results. Its badge counts unread alerts, not resources.

- Command-click live work to toggle selection; stopped and non-Terminal
  records remain selectable in the same sidebar. **Select All** selects a
  Session's members.
- Drag a resource (or its selected group) onto a Session, or use **Move…** /
  `Cmd-Shift-M` and choose a named Session or **Leave unorganized (Intake)**.
  The designated Intake cannot be killed; terminate individual processes or
  move them to another Session instead. **Clear** cancels selection.
- After a committed edit, **Undo** / `Cmd-Option-Z` requests its receipt's
  inverse through the shared runtime. A conflict, changed permission, expired
  receipt, or missing original Session is a refusal, not a fabricated success.

One desktop move is atomic and limited to 256 resources. It changes membership,
never execution identity or process lifetime; closing a local View still only
detaches it. Only the latest organization edit submitted by that window is
offered for undo. Local tabs remain a View arrangement, not a shared-arrangement
editor; Browser resources and cross-machine Workspaces are not implemented.

Session names are labels, not identity. Rename dialogs keep their original
target even if another client renames it or reuses its old name. Home and Quick
Window follow the same session, and reconnect never recreates its old name.
Default launches save layouts by the permanent session key, importing existing
name-based layouts without deleting them; explicit layout paths stay unchanged.
Unreadable or newer saved layouts remain protected from writes.

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

**Current macOS build:** **Open Folder** calls `phux project open` off the UI
thread. One persistent Session is bound to each canonical checkout path;
untrusted [project recipes](../CONFIG.md#project-recipes) are refused until
approved, and a same-name checkout cannot take over another Session. The
sidebar groups Sessions by their recorded project label (shared across Git
worktrees), with **Open** focusing an existing project window or restoring its
per-session layout. A new terminal in that Session starts at its recorded
directory; reopening after the last shell exits starts a shell there without
rerunning the recipe. **Recent folders** keeps the last eight successfully
opened local checkout paths in socket-scoped desktop state across app restarts.
Selecting one runs the same CLI trust and collision checks; **Forget folder**
removes only its shortcut, not the Session, layout, or checkout. This is
local-only: multi-folder or cross-host project references, offline remote
folders, and cross-server Workspaces are not yet implemented.

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

**Go to Terminal** (⌘P) finds an existing placement and focuses its window.
**Mirror Pane** creates a new view of the same terminal in a chosen
split, tab, or window. Scrolling,
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
| Terminate Terminal Process… | Explicitly request server termination; all views reflect the terminal's closure. |
| Kill Session | After confirmation, end every member and delete the session in one server command; report how many ended, or why it was refused. |
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

**Current macOS build:** Sidebar rows keep one title and a restrained status
marker. Hover for directory, provider, lifecycle age, and identity. **Needs
you** keeps explicit blocked, finished, and attention states; context menus
and organization actions remain available.

## Connections, settings, and native behavior

Local and registered remote hosts open concurrently with host-qualified
identity, through the shared registry and dialer; auth refusal, expiry,
version mismatch, and offline inventory are distinct states. Settings show
effective value, default, source, and when an edit applies, using the shared
catalogue and comment-preserving writer. Native menus, Dock, dialogs, drops,
clipboard, notifications, scale, appearance, reduced motion, high contrast,
and screen-reader navigation are in scope; titles, links, and dropped paths
never become implicit command execution.

**Current macOS build:** Appearance puts font family, size, a live sample,
and themes first. The searchable font chooser offers installed families and
the bundled Nerd family, retains the current selection, and identifies a
missing font's Paper Mono substitution. Nerd symbols fall back automatically;
the main font does not need to change. Font assets and patch attribution are
shipped under their [Open Font License](../../clients/desktop/native/assets/fonts/JetBrainsMono-OFL.txt).

Advanced holds line height, cell width, padding, and unfocused-pane dimming.
Display edits apply live and persist with the window's layout; font selection
follows resets and Ghostty imports while Settings stays open.

## Status

The host is a GPUI application. Tracking IDs below name remaining product work,
not delivered features. Projects/worktree grouping, remote hosts, approvals,
notarization, accessibility journeys, and Linux are not shipped.

| Target | Current evidence and gap | Tracked work |
|---|---|---|
| Native GPUI desktop | The executable and packaged smoke are the acceptance record. A presented frame is required; this page does not claim that run. | phux-6hr7 |
| Terminal-first projects and workspace | Tabs, split trees, restore by server identity, Open Folder. Projects/worktree grouping is not built. | phux-d4x9.7–.9 |
| Independent same-terminal views | Mirror Pane, sole-or-last-focused size owner, per-view find. GPU evidence still required. | phux-d4x9.17, phux-d4x9.18 |
| Connections, settings, and agents | One local socket per window; `Phux.app` opens the owner's Intake (else the most recent session) and never creates `default`; display settings and Ghostty-config import; agent badges and one notification feed behind the titlebar bell, whose agent and error entries peek in the window and reach macOS Notification Center while Phux is in the background. Remote hosts, approvals, the inspector, and catalogue-backed settings (effective value, default, source) are not built. | phux-d4x9.10–.12 |
| Native accessibility and delivery safety | Required journeys and teardown/input proofs are not yet qualified. | phux-d4x9.13, phux-d4x9.14 |
| Apple-silicon release | `just desktop-install-app` builds an ad-hoc-signed local bundle with no NAPI addon. In-app verified updates ship; notarization remains required. | phux-d4x9.15, phux-d4x9.16 |
| Linux | Subsequent platform qualification. macOS evidence does not establish it. | phux-d4x9.19 |

## Where to go next

- [Desktop architecture](../architecture/desktop.md) owns runtime and binding seams.
- [Desktop verification](../architecture/desktop-verification.md) owns acceptance evidence.
- [Contributor setup](../SETUP.md) owns environment provisioning.
