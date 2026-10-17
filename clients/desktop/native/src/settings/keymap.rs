//! Chord normalization and the built-in command chords.
//!
//! Ghostty binds overlay these only while display preferences keep
//! `ghostty_keys`. A `global:` bind of `quick-terminal` is the system-wide
//! quick terminal chord.

use super::ghostty::{Bind, GhosttyConfig, Origin};
use std::sync::LazyLock;

/// One built-in command the palette and keymap share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    /// Stable id.
    pub id: &'static str,
    /// Palette title.
    pub title: &'static str,
    /// Short explanation of what running the command does.
    pub description: &'static str,
    /// Additional search terms, beyond the title and description.
    pub keywords: &'static [&'static str],
    /// Palette group.
    pub group: &'static str,
    /// Primary chord, if any.
    pub chord: Option<&'static str>,
    /// Extra chords for the same command.
    pub aliases: &'static [&'static str],
}

impl CommandSpec {
    /// `tab-1` through `tab-9`: bound to chords, but not listed in the
    /// palette or menus.
    #[must_use]
    pub fn is_tab_jump(&self) -> bool {
        self.id
            .strip_prefix("tab-")
            .is_some_and(|rest| rest.parse::<u8>().is_ok())
    }

    /// Listed in the palette. Tab jumps, find steps and undo stay on their
    /// chords and controls.
    #[must_use]
    pub fn is_listed(&self) -> bool {
        !self.is_tab_jump() && !CHORD_ONLY.contains(&self.id)
    }

    /// Needs a server that organizes work into sessions.
    #[must_use]
    pub fn needs_organization(&self) -> bool {
        self.group == "Organization"
    }
}

/// Commands reached by chord or an on-screen control, never by the palette.
const CHORD_ONLY: &[&str] = &[
    "find-next",
    "find-prev",
    "find-selection",
    "find-close",
    "undo-organization",
];

/// Modifiers as the window reports them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KeyChord {
    /// Produced or physical key name.
    pub key: String,
    /// Key repeat.
    pub held: bool,
    /// Command.
    pub cmd: bool,
    /// Option/Alt.
    pub alt: bool,
    /// Control.
    pub ctrl: bool,
    /// Shift.
    pub shift: bool,
}

impl KeyChord {
    /// A key event.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ..Self::default()
        }
    }
}

const UNSHIFTED: &[(&str, &str)] = &[
    ("}", "]"),
    ("{", "["),
    ("+", "="),
    ("_", "-"),
    ("|", "\\"),
    ("?", "/"),
    ("<", ","),
    (">", "."),
];

/// Built-in commands, in palette order. Tab jumps are `tab-1` through `tab-9`.
/// The one catalog: palette, menus, key bindings and Ghostty binds read it.
#[must_use]
pub fn builtin_commands() -> &'static [CommandSpec] {
    static COMMANDS: LazyLock<Vec<CommandSpec>> = LazyLock::new(catalog);
    &COMMANDS
}

/// One command per line: id | title | group | chords | description | keywords.
/// The first chord is primary and the rest are aliases; keywords are
/// space-separated.
const CATALOG: &str = "\
    palette | Command Palette | General | cmd+shift+p | Search and run desktop commands. |
    goto | Go To… | General | cmd+p | Find and focus a terminal, agent or tab. | switch navigate
    settings | Settings… | General | cmd+, | Adjust appearance, key bindings and desktop preferences. | preferences theme
    new | New Terminal | Terminal | cmd+t | Open a new terminal in its own tab. | shell
    native-agent | New Agent… | Terminal |  | Launch a host-owned ACP agent. | agent acp
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
    close | Close Pane | Panes | cmd+w | Detach this pane; its terminal keeps running. |
    close-tab | Close Tab | Tabs | cmd+shift+w | Detach this tab; its terminals keep running. |
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
    sidebar | Toggle Sidebar | View | cmd+b | Show or hide the navigation sidebar. |
    plain-terminal | Toggle Plain Terminal | View | cmd+ctrl+t | Switch between the glass desk and a plain terminal window. | skin minimal ghostty chrome glass
    tabs-top | Tabs on Top | View |  | Place workspace tabs in the titlebar. | layout horizontal placement
    tabs-side | Tabs on Side | View |  | Place workspace tabs in a scrollable left rail. | layout vertical placement
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
    copy | Copy | Terminal |  | Copy the terminal selection to the clipboard. |
    paste | Paste | Terminal |  | Insert clipboard text into the focused terminal. |
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

fn catalog() -> Vec<CommandSpec> {
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

/// Built-in chord to command id, primary chord before aliases.
#[must_use]
pub fn default_chords() -> Vec<(String, String)> {
    let mut chords = Vec::new();
    // Keep Super bindings available, but put terminal-native Linux shortcuts
    // first so menus show them. Plain Ctrl-C/Z/D stay with the terminal.
    #[cfg(target_os = "linux")]
    chords.extend(
        [
            ("ctrl+shift+p", "palette"),
            ("ctrl+shift+t", "new"),
            ("ctrl+shift+w", "close"),
            ("ctrl+shift+n", "new-window"),
            ("ctrl+shift+f", "find"),
            ("ctrl+shift+c", "copy"),
            ("ctrl+shift+v", "paste"),
        ]
        .map(|(chord, id)| (chord.to_owned(), id.to_owned())),
    );
    for command in builtin_commands() {
        if let Some(chord) = command.chord {
            chords.push((chord.to_owned(), command.id.to_owned()));
        }
        for alias in command.aliases {
            chords.push(((*alias).to_owned(), command.id.to_owned()));
        }
    }
    chords
}

/// A command a Ghostty bind can name beyond the catalog.
#[derive(Debug, Clone, PartialEq)]
pub enum BoundAction {
    /// Swallow the key.
    Ignore,
    /// Type `text` as keys. A leading ESC is Alt.
    Send(String),
    /// Write a region to a file, then copy, paste, or open the path.
    WriteFile {
        /// `screen`, `scrollback`, or `selection`.
        region: String,
        /// `copy`, `paste`, or `open`.
        mode: String,
    },
    /// Set the font size.
    FontSize(f64),
    /// Scroll by a line count.
    ScrollLines(f64),
    /// Jump by a prompt count.
    PromptJump(f64),
}

/// Parse `ignore`, `send:`, `write-file:`, `font-size:`, `scroll-lines:`, and
/// `prompt-jump:`. Catalog ids are not returned here.
#[must_use]
pub fn classify_command(id: &str) -> Option<BoundAction> {
    if id == "ignore" {
        return Some(BoundAction::Ignore);
    }
    if let Some(text) = id.strip_prefix("send:") {
        return Some(BoundAction::Send(text.to_owned()));
    }
    if let Some(rest) = id.strip_prefix("write-file:") {
        let mut parts = rest.splitn(2, ':');
        let region = parts.next().unwrap_or("");
        let mode = parts.next().unwrap_or("");
        if matches!(region, "screen" | "scrollback" | "selection")
            && matches!(mode, "copy" | "paste" | "open")
        {
            return Some(BoundAction::WriteFile {
                region: region.to_owned(),
                mode: mode.to_owned(),
            });
        }
        return None;
    }
    let (name, value) = id.split_once(':')?;
    let number = value.parse::<f64>().ok()?;
    if !number.is_finite() {
        return None;
    }
    match name {
        "font-size" => Some(BoundAction::FontSize(number)),
        "scroll-lines" => Some(BoundAction::ScrollLines(number)),
        "prompt-jump" => Some(BoundAction::PromptJump(number)),
        _ => None,
    }
}

fn known_command(id: &str) -> bool {
    builtin_commands().iter().any(|command| command.id == id) || classify_command(id).is_some()
}

/// Built-in chords, then Ghostty binds and unbinds when `ghostty_keys`.
#[must_use]
pub fn effective_keymap(ghostty: &GhosttyConfig, ghostty_keys: bool) -> Vec<(String, String)> {
    let mut map = default_chords();
    if !ghostty_keys {
        return map;
    }
    for (chord, id) in &ghostty.keybinds {
        if id.is_empty() || !known_command(id) {
            map.retain(|(existing, _)| existing != chord);
            continue;
        }
        if let Some(slot) = map.iter_mut().find(|(existing, _)| existing == chord) {
            slot.1.clone_from(id);
        } else {
            map.push((chord.clone(), id.clone()));
        }
    }
    map
}

/// One chord the Ghostty config binds to more than one action. The last bind wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyConflict {
    /// Normalized chord.
    pub chord: String,
    /// Every bind of the chord since the last `keybind = clear`, in config
    /// order; the last is the one in effect.
    pub binds: Vec<Bind>,
}

/// Chords the Ghostty config binds to different actions, in first-bind order.
/// Repeating the same action is not a conflict. An `unbind` is deliberate, so
/// it is never a conflict itself and it drops the binds before it: only binds
/// after a chord's last unbind can conflict.
#[must_use]
pub fn keymap_conflicts(config: &GhosttyConfig) -> Vec<KeyConflict> {
    let mut chords: Vec<&str> = Vec::new();
    for bind in &config.binds {
        if !chords.contains(&bind.chord.as_str()) {
            chords.push(&bind.chord);
        }
    }
    chords
        .into_iter()
        .filter_map(|chord| {
            let binds = live_binds(config, chord);
            let first = binds.first()?;
            binds
                .iter()
                .any(|other| other.command != first.command)
                .then(|| KeyConflict {
                    chord: chord.to_owned(),
                    binds,
                })
        })
        .collect()
}

/// Binds of `chord` after its last unbind, in config order.
fn live_binds(config: &GhosttyConfig, chord: &str) -> Vec<Bind> {
    let mut binds = Vec::new();
    for bind in config.binds.iter().filter(|bind| bind.chord == chord) {
        if bind.command.is_empty() {
            binds.clear();
        } else {
            binds.push(bind.clone());
        }
    }
    binds
}

/// Where a keymap entry came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChordSource {
    /// The built-in catalog.
    BuiltIn,
    /// A `keybind` line.
    Ghostty(Origin),
}

impl ChordSource {
    /// `Built-in`, or `Ghostty ~/.config/ghostty/config:12`.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::BuiltIn => "Built-in".to_owned(),
            Self::Ghostty(origin) => format!("Ghostty {}", origin.label()),
        }
    }
}

/// The source of `chord` running `id` in [`effective_keymap`]: the last
/// Ghostty bind of the chord while `ghostty_keys` and it names `id`, else built-in.
#[must_use]
pub fn chord_source(
    config: &GhosttyConfig,
    ghostty_keys: bool,
    chord: &str,
    id: &str,
) -> ChordSource {
    if !ghostty_keys {
        return ChordSource::BuiltIn;
    }
    config
        .binds
        .iter()
        .rev()
        .find(|bind| bind.chord == chord)
        .filter(|bind| bind.command == id)
        .map_or(ChordSource::BuiltIn, |bind| {
            ChordSource::Ghostty(bind.origin.clone())
        })
}

/// Chords without Command. The terminal must hand these to the window.
#[must_use]
pub fn app_chords(keymap: &[(String, String)]) -> Vec<String> {
    keymap
        .iter()
        .filter(|(chord, _)| !chord.starts_with("cmd+"))
        .map(|(chord, _)| chord.clone())
        .collect()
}

/// The chord to show for each command. A user bind wins over a built-in one.
#[must_use]
pub fn shown_chords(
    keymap: &[(String, String)],
    defaults: &[(String, String)],
) -> Vec<(String, String)> {
    let built_in = |chord: &str, id: &str| -> bool {
        defaults
            .iter()
            .any(|(candidate, command)| candidate == chord && command == id)
    };
    let mut shown: Vec<(String, String)> = Vec::new();
    for (chord, id) in keymap {
        if let Some(slot) = shown.iter_mut().find(|(command, _)| command == id) {
            if !built_in(chord, id) && built_in(&slot.1, id) {
                slot.1.clone_from(chord);
            }
        } else {
            shown.push((id.clone(), chord.clone()));
        }
    }
    shown
}

/// System-wide quick-terminal chords: `global:` binds of `quick-terminal`.
#[must_use]
pub fn quick_terminal_chords(config: &GhosttyConfig) -> Vec<String> {
    config
        .globals
        .iter()
        .filter(|chord| {
            config
                .keybinds
                .get(*chord)
                .is_some_and(|id| id == "quick-terminal")
        })
        .cloned()
        .collect()
}

/// Saved `ghosttyKeys: false` suppresses global binds. A missing snapshot does not.
#[must_use]
pub fn register_global_hotkeys(ghostty_keys: Option<bool>) -> bool {
    ghostty_keys != Some(false)
}

/// Command chord, or empty when the app does not arbitrate the key.
#[must_use]
pub fn chord_of(event: &KeyChord) -> String {
    if !event.cmd || event.key.is_empty() || event.held {
        return String::new();
    }
    let (key, shifted) = normalize_key(&event.key);
    join_chord(true, event.ctrl, event.alt, event.shift || shifted, &key)
}

/// Any key as a chord. An unmodified printable is typing, not a chord.
#[must_use]
pub fn key_chord(event: &KeyChord) -> String {
    if event.key.is_empty() || event.held {
        return String::new();
    }
    if event.cmd {
        return chord_of(event);
    }
    let (key, shifted) = normalize_key(&event.key);
    let shift = event.shift || shifted;
    if !event.ctrl && !event.alt && !shift && key.chars().count() == 1 {
        return String::new();
    }
    if !event.ctrl && !event.alt && key == "escape" {
        return "escape".to_owned();
    }
    join_chord(false, event.ctrl, event.alt, shift, &key)
}

/// Overlay navigation key, modifiers ignored.
#[must_use]
pub fn plain_key(key: &str) -> String {
    key.to_lowercase()
}

/// Platform modifier labels, followed by the key.
#[must_use]
pub fn display_chord(chord: &str) -> String {
    #[cfg(target_os = "linux")]
    {
        chord
            .split('+')
            .map(|part| match part {
                "cmd" => "Super".to_owned(),
                "ctrl" => "Ctrl".to_owned(),
                "alt" => "Alt".to_owned(),
                "shift" => "Shift".to_owned(),
                key => key.to_uppercase(),
            })
            .collect::<Vec<_>>()
            .join("+")
    }
    #[cfg(not(target_os = "linux"))]
    display_apple_chord(chord)
}

#[cfg(not(target_os = "linux"))]
fn display_apple_chord(chord: &str) -> String {
    let parts: Vec<&str> = chord.split('+').collect();
    let key = parts.last().copied().unwrap_or("");
    let mut shown: String = ["ctrl", "alt", "shift", "cmd"]
        .into_iter()
        .filter(|modifier| parts.contains(modifier))
        .map(glyph)
        .collect();
    let label = match glyph(key) {
        same if same != key => same.to_owned(),
        _ if key.chars().count() == 1 => key.to_uppercase(),
        _ => key.to_owned(),
    };
    shown.push_str(&label);
    shown
}

#[cfg(not(target_os = "linux"))]
fn glyph(name: &str) -> &str {
    match name {
        "cmd" => "⌘",
        "ctrl" => "⌃",
        "alt" => "⌥",
        "shift" => "⇧",
        "enter" => "↩",
        "up" => "↑",
        "down" => "↓",
        "left" => "←",
        "right" => "→",
        "escape" => "esc",
        "backspace" => "⌫",
        other => other,
    }
}

/// A dialog, palette, or the focused pane's find bar owns editing keys.
#[must_use]
pub fn text_field_has_keys(
    modal_open: bool,
    find_placement_id: Option<&str>,
    focused_placement_id: Option<&str>,
) -> bool {
    modal_open || find_placement_id.is_some_and(|find| Some(find) == focused_placement_id)
}

/// Select All and Ghostty text/esc binds stay with a text field.
#[must_use]
pub fn yields_to_text_field(command_id: &str) -> bool {
    command_id == "select-all" || command_id.starts_with("send:")
}

/// The key with shifted punctuation folded to its base key, and whether the
/// raw key implied Shift.
fn normalize_key(raw: &str) -> (String, bool) {
    let lowered = raw.to_lowercase();
    let key = UNSHIFTED
        .iter()
        .find(|(shifted, _)| *shifted == lowered)
        .map_or_else(|| lowered.clone(), |(_, plain)| (*plain).to_owned());
    let shifted = key != lowered || lowered != raw;
    (key, shifted)
}

/// `cmd+ctrl+alt+shift+key`, with only the modifiers held.
pub(super) fn join_chord(cmd: bool, ctrl: bool, alt: bool, shift: bool, key: &str) -> String {
    [(cmd, "cmd"), (ctrl, "ctrl"), (alt, "alt"), (shift, "shift")]
        .into_iter()
        .filter_map(|(held, name)| held.then_some(name))
        .chain([key])
        .collect::<Vec<_>>()
        .join("+")
}

#[cfg(test)]
mod tests {
    use super::super::ghostty::parse_ghostty;
    use super::{ChordSource, chord_source, default_chords, effective_keymap, keymap_conflicts};

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_shortcuts_leave_shell_control_keys_alone() {
        let chords = default_chords();
        assert!(chords.contains(&("ctrl+shift+c".into(), "copy".into())));
        assert!(chords.contains(&("ctrl+shift+v".into(), "paste".into())));
        for key in ["ctrl+c", "ctrl+z", "ctrl+d"] {
            assert!(!chords.iter().any(|(chord, _)| chord == key));
        }
        assert_eq!(super::display_chord("ctrl+shift+c"), "Ctrl+Shift+C");
        assert_eq!(super::display_chord("cmd+t"), "Super+T");
    }

    #[test]
    fn built_in_chords_never_conflict() {
        let chords = default_chords();
        for (index, (chord, id)) in chords.iter().enumerate() {
            assert!(
                !chords[index + 1..]
                    .iter()
                    .any(|(other, other_id)| other == chord && other_id != id),
                "{chord} is bound to more than one built-in command"
            );
        }
    }

    #[test]
    fn a_chord_bound_to_different_actions_is_a_conflict() {
        let config = parse_ghostty(
            "keybind = super+d=new_split:right\n\
             keybind = super+k=clear_screen\n\
             keybind = super+d=new_split:down\n\
             keybind = super+k=clear_screen\n\
             keybind = super+j=new_tab\n",
        );
        let conflicts = keymap_conflicts(&config);
        assert_eq!(
            conflicts.len(),
            1,
            "a repeated identical bind is not a conflict"
        );
        let conflict = &conflicts[0];
        assert_eq!(conflict.chord, "cmd+d");
        let lines: Vec<(usize, &str)> = conflict
            .binds
            .iter()
            .map(|bind| (bind.origin.line, bind.command.as_str()))
            .collect();
        assert_eq!(lines, vec![(1, "split-right"), (3, "split-down")]);
        // The last line is the one in effect, and the keymap agrees.
        let keymap = effective_keymap(&config, true);
        assert!(keymap.contains(&("cmd+d".to_owned(), "split-down".to_owned())));
        assert_eq!(
            chord_source(&config, true, "cmd+d", "split-down"),
            ChordSource::Ghostty(conflict.binds[1].origin.clone())
        );
        assert_eq!(
            chord_source(&config, false, "cmd+d", "split-right"),
            ChordSource::BuiltIn
        );
        assert_eq!(
            chord_source(&config, true, "cmd+t", "new"),
            ChordSource::BuiltIn
        );
    }

    #[test]
    fn an_unbind_is_not_a_conflict() {
        let config = parse_ghostty(
            "keybind = super+d=new_split:right\n\
             keybind = super+d=unbind\n\
             keybind = super+k=clear_screen\n\
             keybind = super+k=unbind\n\
             keybind = super+k=new_tab\n\
             keybind = super+j=new_tab\n\
             keybind = super+j=unbind\n\
             keybind = super+j=clear_screen\n\
             keybind = super+j=new_split:down\n",
        );
        let conflicts = keymap_conflicts(&config);
        let chords: Vec<&str> = conflicts.iter().map(|c| c.chord.as_str()).collect();
        assert_eq!(chords, vec!["cmd+j"], "an unbind resets the chord");
        let lines: Vec<usize> = conflicts[0]
            .binds
            .iter()
            .map(|bind| bind.origin.line)
            .collect();
        assert_eq!(lines, vec![8, 9]);
    }

    #[test]
    fn clear_and_built_in_restores_drop_earlier_binds() {
        let config = parse_ghostty(
            "keybind = super+d=new_split:right\n\
             keybind = clear\n\
             keybind = super+d=new_split:down\n\
             keybind = super+c=new_tab\n\
             keybind = super+c=copy_to_clipboard\n",
        );
        assert!(keymap_conflicts(&config).is_empty());
        assert_eq!(config.binds.len(), 1);
        assert_eq!(config.binds[0].origin.line, 3);
    }
}
