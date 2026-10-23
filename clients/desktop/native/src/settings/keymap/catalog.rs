//! The built-in command catalog: one row per command the palette, menus,
//! key bindings and Ghostty binds share.

use super::CommandSpec;

/// One command per line: id | title | group | chords | description | keywords.
/// The first chord is primary and the rest are aliases; keywords are
/// space-separated.
const CATALOG: &str = "\
    palette | Command Palette | General | cmd+shift+p | Search and run desktop commands. |
    capture | Capture… | General | cmd+shift+space | Dispatch a thought to OpenCode. Enter follows; Command/Control+Enter stays; Shift+Enter adds a line. | thought idea bug intake
    goto | Go To… | General | cmd+p | Find and focus a terminal, agent or tab. | switch navigate
    settings | Settings… | General | cmd+, | Adjust appearance, key bindings and desktop preferences. | preferences theme
    new | New Terminal | Terminal | cmd+t | Open a new terminal in its own tab. | shell
    native-agent | New Agent… | Terminal |  | Start a native agent in the focused folder, on this host. | agent acp opencode remote headless login
    new-session | New Session… | Terminal | cmd+shift+n | Create a named session. |
    rename-session | Rename Session… | Terminal |  | Change the current session's name. |
    select-session-work | Select All in Session | Organization |  | Select the current session's work for organization. |
    move-resources | Move to Session… | Organization | cmd+shift+m | Choose a session for the selected work. |
    undo-organization | Undo Organization | Organization | cmd+alt+z | Reverse the last organization change. |
    organize-everything | Organize Everything | Organization | cmd+shift+o | Move every pane into the session for its repository or task. | tidy sort group
    undo-organize-everything | Undo Organize Everything | Organization |  | Reverse the last Organize Everything run. |
    kill-session | Kill Session… | Terminal |  | Confirm termination of a session and its processes. | delete
    new-home | New Terminal in Home Directory | Terminal | cmd+shift+t | Open a new terminal starting in your home directory. | shell
    folder | Open Folder… | Terminal | cmd+o | Choose a folder for a new terminal. | directory project
    insert-path | Insert Path… | Terminal | cmd+shift+i | Choose a file or folder path to insert into the terminal. |
    split-right | Split Right | Panes | cmd+d | Open a terminal beside the focused pane. | vertical
    split-down | Split Down | Panes | cmd+shift+d | Open a terminal below the focused pane. | horizontal
    split-left | Split Left | Panes |  | Open a terminal to the left of the focused pane. |
    split-up | Split Up | Panes |  | Open a terminal above the focused pane. |
    view | Mirror Pane | Panes | cmd+alt+d | Show the focused terminal in another pane. | duplicate open another view
    close | Close Pane | Panes | cmd+w | End this pane's process and remove all its views. |
    close-tab | Close Tab | Tabs | cmd+shift+w | End every process in this tab and remove their views. |
    zoom | Zoom Pane | Panes | cmd+shift+enter | Expand the focused pane or restore its splits. |
    equalize | Equalize Splits | Panes | cmd+ctrl+= | Give the active tab's panes equal space. | balance
    pane-left | Focus Pane Left | Panes | cmd+alt+left | Focus the neighboring pane on the left. |
    pane-right | Focus Pane Right | Panes | cmd+alt+right | Focus the neighboring pane on the right. |
    pane-up | Focus Pane Up | Panes | cmd+alt+up | Focus the neighboring pane above. |
    pane-down | Focus Pane Down | Panes | cmd+alt+down | Focus the neighboring pane below. |
    pane-next | Next Pane | Panes | cmd+] | Focus the next pane in the active tab. |
    pane-prev | Previous Pane | Panes | cmd+[ | Focus the previous pane in the active tab. |
    tab-next | Next Tab | Tabs | cmd+shift+] | Switch to the next tab. |
    tab-prev | Previous Tab | Tabs | cmd+shift+[ | Switch to the previous tab. |
    tab-peek | Peek Tabs | Tabs | cmd+shift+\\ | Show live terminal miniatures without resizing your work. Escape closes; arrows browse; Enter opens. | preview thumbnails overview
    tab-move-left | Move Tab Left | Tabs |  | Move the active tab earlier in the tab bar. | reorder
    tab-move-right | Move Tab Right | Tabs |  | Move the active tab later in the tab bar. | reorder
    rename | Rename Tab… | Tabs |  | Set a custom name for the active tab. |
    window | Move Pane to New Window | Panes | cmd+alt+n | Move the focused terminal into a separate window. | detach
    window-next | Move Pane to Next Window | Panes |  | Move the focused terminal into the next open window on the same server. | window transfer
    find | Find in Terminal | Terminal | cmd+f | Search the focused terminal's output. | search scrollback
    find-next | Find Next | Terminal | cmd+g | Select the next search match. |
    find-prev | Find Previous | Terminal | cmd+shift+g | Select the previous search match. |
    find-selection | Use Selection for Find | Terminal | cmd+e | Search for the selected terminal text. |
    find-close | Close Find | Terminal |  | Dismiss the terminal search bar. |
    follow | Scroll to Live Output | Terminal | cmd+l | Return to the latest terminal output. | bottom
    paste-cwd | Paste Working Directory | Terminal |  | Insert the focused terminal's working directory. | cwd path
    terminate | Terminate Terminal Process… | Terminal |  | Confirm termination of the focused terminal process. | kill
    attention | Jump to Agent Needing Attention | Agents | cmd+shift+a | Focus the next agent that needs your attention. |
    agent-fleet | Agent Fleet | Agents |  | Open the live agent overview. Enable Agent fleet in Settings > Layout & notifications first. | dashboard overview grid mission control
    native-agents | Toggle Native Agents | Agents | cmd+ctrl+a | Snap every agent pane to its native conversation, or back to its terminal. | conversation chat transcript acp tui harness native
    conversation | Toggle Conversation | Agents |  | Show the focused agent pane's conversation, or its terminal. | chat transcript native acp
    sidebar | Toggle Sidebar | View | cmd+b | Show or hide the navigation sidebar. |
    changes | Toggle Changes | View | cmd+alt+g | Show the focused terminal's Git changes beside the panes: diffs, staging and commit. | git diff stage commit status branch review
    plain-terminal | Toggle Plain Terminal | View | cmd+ctrl+t | Switch between the glass desk and a plain terminal window. | skin minimal ghostty chrome glass
    tabs-top | Tabs on Top | View |  | Place workspace tabs in the titlebar. | layout horizontal placement
    tabs-bottom | Tabs on Bottom | View |  | Place workspace tabs below your work, including in plain terminal mode. | layout horizontal placement
    tabs-side | Tabs in Sidebar | View |  | List workspace tabs in the sidebar’s fixed tab area, or a rail of their own in plain terminal mode. | layout vertical placement side rail
    group-projects | Group Sessions by Project | View |  | List sessions under their repository in the navigator. | view organize
    group-machines | Group Sessions by Machine | View |  | List sessions under the machine they run on in the navigator. | view hosts organize
    group-none | Ungroup Sessions | View |  | List every session in one navigator list without headings. | view flat
    expand-sessions | Expand All Sessions | View |  | Show the terminals of every session in the navigator. | sidebar unfold open tree
    collapse-sessions | Collapse All Sessions | View |  | Fold every session in the navigator down to its name. | sidebar fold close tree tidy
    main-menu | Show Main Menu | General |  | Open the session menu beside the window controls. | hub workspace switch machine
    font-up | Increase Font Size | View | cmd+= cmd+shift+= | Make terminal text larger. |
    font-down | Decrease Font Size | View | cmd+- | Make terminal text smaller. |
    font-reset | Reset Font Size | View | cmd+0 | Restore the default terminal text size. |
    notifications | Show Inbox & Notifications | View |  | Review inbox entries and desktop notices. | alerts
    clear-notifications | Clear Inbox & Notifications | View |  | Remove all entries from the notification feed. |
    resize-left | Move Divider Left | Panes |  | Nudge the focused pane's divider to the left. |
    resize-right | Move Divider Right | Panes |  | Nudge the focused pane's divider to the right. |
    resize-up | Move Divider Up | Panes |  | Nudge the focused pane's divider upward. |
    resize-down | Move Divider Down | Panes |  | Nudge the focused pane's divider downward. |
    scroll-page-up | Scroll Page Up | Terminal | cmd+pageup | Scroll terminal output up by one page. |
    scroll-page-down | Scroll Page Down | Terminal | cmd+pagedown | Scroll terminal output down by one page. |
    scroll-top | Scroll to Top | Terminal | cmd+home | Scroll to the oldest available terminal output. |
    scroll-bottom | Scroll to Bottom | Terminal | cmd+end | Return to the latest terminal output. |
    clear | Clear Screen | Terminal | cmd+k | Send the clear-screen control to the terminal. |
    select-all | Select All | Terminal | cmd+a | Select all available terminal text. |
    copy | Copy | Terminal | cmd+c | Copy the terminal selection to the clipboard. |
    paste | Paste | Terminal | cmd+v | Insert the clipboard into the focused terminal; a copied file or image goes in as its path. |
    paste-selection | Paste Selection | Terminal |  | Insert the selected terminal text as input. |
    prompt-prev | Jump to Previous Prompt | Terminal | cmd+up | Scroll to the previous shell prompt marker. |
    prompt-next | Jump to Next Prompt | Terminal | cmd+down | Scroll to the next shell prompt marker. |
    write-file:scrollback:open | Open Scrollback as a File | Terminal |  | Write terminal scrollback to a file and open it. | export editor
    quick-terminal | Toggle Quick Terminal | View |  | Open or close the quick-terminal window. | dropdown hotkey
    new-window | New Window | View | cmd+n | Open another window on a session. |
    fullscreen | Toggle Full Screen | View | cmd+ctrl+f | Enter or leave native full-screen mode. |
    maximize | Maximize Window | View |  | Toggle the native window's zoomed size. |
    minimize | Minimize | View | cmd+m | Send this window to the Dock. | hide window
    windows | Show All Windows… | View |  | List every open window with its session, host and tabs, and bring one forward. | overview switch minimized tabs
    reload-config | Reload Ghostty Config | Ghostty | cmd+shift+, | Read the Ghostty configuration again. |
    import-ghostty | Use Ghostty Font, Colors and Padding | Ghostty |  | Apply Ghostty appearance settings to this window. |
    reconnect | Reconnect to Server | Connection | cmd+shift+r | Restart this window's connection to the server. |
    machines | Machines… | Connection |  | List this computer and every saved machine with its connection state; open, retry, disconnect or forget one. | hosts remote servers ssh connect status
    add-host | Add Machine… | Connection |  | Enroll a machine over ssh so its sessions appear in Work. | host remote server ssh enroll pair connect
    performance-monitor | Performance Monitor | Utilities |  | Inspect desktop CPU, memory and render timings. | telemetry cpu memory rss
    copy-diagnostics | Copy Diagnostics | Utilities |  | Copy app, runtime, server and connection identity with recent errors. No terminal content. | support debug report version log
    performance-monitor-stop | Stop Performance Monitoring | Utilities |  | Close the performance window and stop its sampler. | telemetry cpu memory rss
    developer-mode | Toggle Developer Mode | Utilities |  | Turn Developer mode on or off and save it: the performance HUD, allocation counting and the dev socket. | dev debug developer telemetry socket profiling diagnostics
    perf-hud | Toggle Performance HUD | Utilities |  | Show frame rate, frame build time, CPU, memory, heap and the server's load over the window. | fps hud overlay telemetry frames jank heap
    perf-trace | Capture Performance Trace | Utilities |  | Record five seconds of desktop and server spans to a Perfetto file and copy its path. | trace perfetto profile timeline spans flamegraph
    tab-1 | Go to Tab | Tabs | cmd+1 | Switch to the tab at this position. |
    tab-2 | Go to Tab | Tabs | cmd+2 | Switch to the tab at this position. |
    tab-3 | Go to Tab | Tabs | cmd+3 | Switch to the tab at this position. |
    tab-4 | Go to Tab | Tabs | cmd+4 | Switch to the tab at this position. |
    tab-5 | Go to Tab | Tabs | cmd+5 | Switch to the tab at this position. |
    tab-6 | Go to Tab | Tabs | cmd+6 | Switch to the tab at this position. |
    tab-7 | Go to Tab | Tabs | cmd+7 | Switch to the tab at this position. |
    tab-8 | Go to Tab | Tabs | cmd+8 | Switch to the tab at this position. |
    tab-9 | Go to Last Tab | Tabs | cmd+9 | Switch to the last tab. |
";

pub(super) fn catalog() -> Vec<CommandSpec> {
    CATALOG
        .lines()
        .map(|line| {
            let mut fields = line.split('|').map(str::trim);
            let mut field = || fields.next().expect("a catalog row has six fields");
            let (id, title, group) = (field(), field(), field());
            let mut chords = field().split_whitespace();
            let chord = chords.next();
            let aliases = chords.collect::<Vec<_>>().leak();
            let description = field();
            let keywords = field().split_whitespace().collect::<Vec<_>>().leak();
            CommandSpec {
                id,
                title,
                description,
                keywords,
                group,
                chord,
                aliases,
            }
        })
        .collect()
}
