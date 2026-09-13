---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-09-12
---

# Cockpit: an excellent everyday terminal

**TL;DR.** Make Cockpit understandable from its first window and dependable
through a full working day. Machines are easy to find and connect to; sessions,
tabs and windows have clear jobs; commands are discoverable; settings are useful;
and returning to work preserves context. This is the approved product contract
for the usability recovery. Implementation acceptance requires observed behavior.

## Summary

Cockpit should feel like one carefully designed native terminal. A person can
find their machines, connect, create and organize work, change preferences, and
return later without learning coordinator internals or reconstructing where
their work went. Feature completeness includes discovery, operation, failure,
and recovery for each ordinary task.

## Design context

Tracking: Beads `phux-2jza`. The user delegates product judgment and explicitly
rejects treating closed implementation tickets as proof of a finished experience.
The user approved implementation of this contract on 2026-09-11 and requested
parallel agents, with the parent responsible for integration and product quality.
This contract extends the earlier [canvas slice](../phux-dhcb/PRODUCT.md), whose
bounded terminal-host filtering and appearance page do not meet that broader goal.

The supplied visual reference is the
[recorded Superlogical journey](../../../../../research/2026-09-09-superlogical-demo/UX.md)
and its source frames. The earlier canvas spec records Figma: none provided;
no additional Figma has been supplied for this recovery. The recording does not
show a settings page. Settings below are a product proposal, not reconstructed
evidence. [Product direction](../../PRODUCT_DIRECTION.md)
and the existing [chrome register](../../DESIGN_SYSTEM.md)
continue to govern presentation.

## Behavior

### Orient and find work

1. A normal terminal window makes the selected session and focused terminal's
   machine legible alongside its tabs. The session has a human-readable name;
   the host is supporting context. Different users on the same host remain
   distinguishable. Unknown host or session state is not labeled as local.
2. A **session** is named work that can survive closing the app. A **tab** shows
   a terminal or split layout. A **window** is a macOS window holding tabs.
   "Workspace" may describe the overall working view, but is not a fourth
   unexplained object a person must create before using a terminal. Mixed-machine
   work remains possible; each focused pane still identifies where it runs.
3. The session control opens **Sessions**. A visible **Machines** action opens
   the machine list. The native Window menu exposes **Show All Windows**.
   These are named views of one navigator, with Sessions, Machines and Windows
   always reachable within it; opening one never requires closing another first.
   These destinations are also searchable commands. Each view uses the same
   interaction conventions without making a machine row masquerade as a tab.
4. Sessions lists named sessions grouped by machine, with the selected session
   marked independently from keyboard highlight. New Session is visible and
   names its destination. A session with no tabs is listed and opens an explicit
   empty state with New Tab. Sessions created through Cockpit keep their names
   and remain available after their last terminal exits. Existing CLI-created
   sessions retain their configured lifetime. An unreachable machine is not an
   empty session.
5. Search finds work by session name, terminal title, directory and machine.
   Results explain what selecting them will do: switch to existing work, open
   an available terminal, or connect to a machine. Selecting existing work brings
   it forward rather than spawning a duplicate.
6. Lists use the available window height, remain scrollable, and keep the
   selected result visible. A large window is not limited to four visible results.
   Background updates preserve the selected identity and scroll position where
   possible; removing that identity clears its action rather than retargeting it.

### Machines and connection

7. Machines opens without typing a hostname first. It shows This Mac and all
   saved machines, including disconnected machines and machines with no known
   sessions. Saved and discovered are different claims: an empty saved list says
   **No remote machines added**, with Add Machine as the next action.
8. Each machine has a recognizable name, address/user detail when needed, and
   a truthful state: Not connected, Connecting, Connected, Reconnecting, or
   Failed with a reason. Being saved does not imply reachability. Unchecked
   reachability is not reported as Offline.
9. Selecting a connected machine reveals its sessions. A disconnected machine
   offers Connect directly. A failed machine offers Retry and the relevant
   remedy. Connection progress stays attached to that machine; unrelated work
   remains usable. Double activation does not create duplicate connections.
10. Add Machine accepts a hostname or SSH destination and a friendly name. It
    explains the next required setup step in context, including authentication,
    missing Phux, or an unsupported connection route. If an interactive terminal
    is required, Cockpit opens a dedicated local setup terminal, retains the
    destination, and provides a clear return-and-recheck action. A bare command
    to copy from an error message is not the complete setup experience.
11. A saved machine is the same machine in the CLI and Cockpit. Editing or adding
    one outside Cockpit becomes visible on refresh or reopening Machines.
    Different user identities stay distinct; ambiguous aliases require a specific
    choice. Refreshing the list never authenticates or changes a connection.
12. Disconnect stops this app's connection without killing remote work. The
    machine stays saved and can be connected again. Forget Machine is a separate
    action that identifies the saved entry it will remove; if still connected,
    it first requires an explicit disconnect. Neither operation silently changes
    a different machine or removes the server's sessions.
13. Neither the saved-machine list nor connected machines has a four-machine
    product ceiling. Adding more machines does not subscribe to every terminal
    or force the person to keep removing entries. If a connection cannot be opened because resources are
    unavailable, existing visible work stays intact and the affected machine
    explains the condition with Retry.
14. Canceling setup or selecting other work cancels any pending request to steal
    focus. A late successful connection may become available in Machines, but
    cannot replace the work the person subsequently chose. Credentials and pins
    are handled through the same trust experience as Phux; nothing silently
    disables verification to make a connection appear to work.

### Tabs, windows and continuity

15. New Tab and Split inherit the focused terminal's machine and meaningful
    working directory. New Session names its target machine. A context menu on a
    different tab acts on that tab, not the focused one. A local settings editor
    is explicitly local even when remote work is focused.
16. Tab titles prioritize useful working context. User names survive command
    title changes; similar titles can be distinguished by session, host and
    directory. Top and side tab placement offer equivalent selection and actions.
    Overflow remains searchable and directly reachable.
17. Tabs support discoverable close, rename and reorder actions through pointer
    interaction and keyboard commands. Split focus is visible. Reordering,
    changing appearance and opening a picker do not restart a terminal or change
    its identity. An operation that cannot be applied reports failure at the
    initiating context instead of appearing to succeed and then jumping back.
18. Show All Windows lists open macOS windows with their selected session/tab
    context and current-window marker. Choosing an entry raises its existing
    window, including a minimized one; choosing one of its tabs selects that tab.
    Empty windows and windows with similar titles remain distinguishable.
    New Window creates a new terminal tab in the invoking session on the focused
    terminal's machine and presents it in the new window. Other windows keep
    their selected tabs; it does not create another named session. With no active
    session, it offers the local session/new-session choice. Selecting a different
    session changes only that window's view. Sessions A and B on the same machine
    can remain visible and usable in separate windows. Viewing the same session
    twice shares its layout and execution; it does not duplicate either.
19. A stopped process, a detached view and a lost connection have different
    presentations and next actions. The following lifecycle applies consistently
    to menu, shortcut, pointer and command-palette actions. Pane/tab closure is
    distinct from closing the client window; controls use these exact names.

    | Action | Phux-backed work | Ephemeral scratch work |
    |---|---|---|
    | Close Pane / Cmd+W with a pane focused | End that terminal and its agent children; remove its pane from the shared session layout. | End that terminal and remove its pane. |
    | Close Tab | End the terminals in that tab and remove its shared layout. | End the terminals in that tab and remove it. |
    | Close Window / macOS close button | Detach this window's views; retain running terminals and session layout. Other windows stay attached. | End the window's scratch terminals. |
    | Disconnect Machine | Detach this app's views of that machine; retain its running work and saved registration. | Does not apply; This Mac is not a remote connection. |
    | Quit | Detach all Phux views; retain running work and layout for reopening. | End this app's scratch terminals. |
    | Terminal process exits | The terminal ends; remaining panes continue. A keep-empty session stays in Sessions when its last terminal exits. | The terminal ends; remaining panes continue. |
    | End Session | End that named session and its terminals on its owning machine; other clients observe the termination. | Does not apply; scratch work is not advertised as a durable session. |

    Closing the last terminal of a keep-empty session shows Empty session with
    New Tab. Otherwise an emptied window closes. Closing the last macOS window
    quits the app. Cmd+W in an Empty session closes that window without ending
    the named session. Scratch work is explicitly labeled; it is never given
    Phux durability wording. A shared terminal closed from another client shows
    an ended state or the resulting Empty session, not a connection error.
20. Quit and relaunch return to the last foreground working context when that
    session still exists. Saved machines remain available even when unreachable.
    Restoring other contexts does not change the person's newer selection.
    Missing or ended work is explained and offers existing alternatives; it is
    not silently replaced by a new session with the same label.
21. A reconnect preserves the last visible output as visibly reconnecting state.
    Input is never silently sent to a different terminal. Once ready, the same
    terminal resumes. A server cold restart is not presented as successful
    recovery of work it no longer retains. After loss is detected, new input is
    refused with a visible Reconnecting state rather than silently queued. Input
    accepted before detection follows Phux's existing replay/deduplication
    contract; when delivery cannot be determined, Cockpit reports it as uncertain
    and does not resubmit it as a fresh command. Speculative local echo is never
    presented as proof of remote execution.

### Commands and first use

22. **Commands** is a searchable action palette with labels, meaningful search
    terms, actual shortcut hints and context. It includes session and tab
    creation, splits, rename, directory navigation, machine/window navigation,
    settings and Edit Configuration. Disabled actions explain their condition.
23. Cmd+Shift+P opens Commands. The existing Go to Terminal action remains
    available within it and from native menus. Cmd+K continues to mean Clear;
    Cmd+Shift+G remains Find Previous. Familiar working shortcuts are preserved
    unless this contract explicitly changes their meaning.
24. Native menus expose ordinary work without requiring any shortcut knowledge.
    The initial empty state gives one useful action, not an introduction to
    internal architecture. Labels and tooltips teach the same vocabulary used
    by the session control, machine list and window overview.
25. Keyboard and pointer invoke the same actions. Search fields support normal
    text editing and composition. Arrows move selection, Enter performs the
    labeled action, Escape dismisses, and focus returns to the invoking terminal
    or a sensible surviving target. Only the visible interaction surface receives
    input. Background output cannot close a picker or steal its selection.

### Settings and editing configuration

26. Settings has searchable, clearly named groups for Appearance, Terminal,
    Keyboard and Window behavior, with common choices directly editable.
    Common choices include font family and size, theme and system-theme following,
    contrast, cursor style/blink, scrollback retention, working-directory
    inheritance, tab placement and the preferred editor. Shell choices identify
    which machine and newly created terminals they affect. Keyboard settings
    exposes the actual bindings, supports remapping and reset, and reports
    conflicting chords before saving, rather than merely displaying hints.
    Each surfaced setting shows its effective value, reset-to-default action,
    and whether it takes effect now, for new terminals, or after restart.
    Application settings are distinguished from Phux server/TUI configuration.
27. Appearance preview is reversible. Cancel restores the opening values; Save
    changes only the settings edited. A failed save leaves the preview and
    actionable error visible. Malformed files, unknown keys, comments and
    concurrent external edits are not discarded to make Save succeed.
28. **Edit Configuration** names the actual target file and opens it in a new
    local Phux-backed terminal with the preferred editor. Editor preference uses
    the configured choice, then VISUAL, then EDITOR; when none is usable, the
    app offers an editor choice rather than silently using Finder. Editor
    arguments and paths containing spaces work as intended. The app does not
    type an editor command into an unrelated running shell.
29. If the config does not yet exist, editing starts from a valid initial file
    at the resolved location. An unresolved or unwritable destination is
    explained. Unsaved Settings previews require Save, Discard or Cancel before
    opening the external editor; the choice affects only those pending changes.
    Reload Configuration reports parsing errors while retaining last-good values.
30. Changing presentation never changes where work runs. Settings stays readable
    during theme preview. Focus indicators, selected states, control labels and
    contrast remain usable with keyboard navigation and assistive technology.

### Installation and day-to-day trust

31. Installing Cockpit leads to a working local Phux terminal, whether Phux was
    already installed or not. Installing Phux later does not unexpectedly move
    existing work to another server. Version incompatibility has an explicit
    recovery path and never silently falls back to ephemeral execution.
32. About and update information distinguish the app version from the connected
    Phux version. Update results identify what changed and whether relaunch is
    required. Repeating the supported installer/update command is safe and
    preserves preferences, machine registrations and ongoing work.
33. Routine success is quiet; failure is specific and recoverable. Normal use
    does not require reading logs. Diagnostics remain available with the actual
    build and connection identity so an observed failure can be investigated.
34. Ordinary terminal interaction remains the foundation: during normal connected
    operation, Cockpit dispatches each input once to the intended terminal;
    uncertain delivery during failure follows behavior 21. Selection, copy/paste, search,
    scrolling, links and split dragging behave consistently with their visible
    targets. Opening navigation, refreshing many machines and loading history
    cannot freeze input or move a pinned scroll position. TUI applications retain
    their own mouse/alternate-screen behavior. Text and focus remain legible on
    the actual macOS display, including during resize and appearance changes.
