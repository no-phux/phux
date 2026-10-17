//! Chord normalization and the built-in command chords.
//!
//! Ghostty binds overlay these only while display preferences keep
//! `ghostty_keys`. A `global:` bind of `quick-terminal` is the system-wide
//! quick terminal chord.

use super::ghostty::GhosttyConfig;
use std::collections::HashMap;
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
            held: false,
            cmd: false,
            alt: false,
            ctrl: false,
            shift: false,
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

fn catalog() -> Vec<CommandSpec> {
    let mut commands = vec![
        spec(
            "palette",
            "Command Palette",
            "General",
            Some("cmd+shift+p"),
            "Search and run desktop commands.",
            &[],
        ),
        spec(
            "goto",
            "Go To…",
            "General",
            Some("cmd+p"),
            "Find and focus a terminal, agent or tab.",
            &["switch", "navigate"],
        ),
        spec(
            "settings",
            "Settings",
            "General",
            Some("cmd+,"),
            "Adjust appearance, key bindings and desktop preferences.",
            &["preferences", "theme"],
        ),
        spec(
            "new",
            "New Terminal",
            "Terminal",
            Some("cmd+t"),
            "Open a new terminal in its own tab.",
            &["shell"],
        ),
        spec(
            "native-agent",
            "New Agent…",
            "Terminal",
            None,
            "Launch a host-owned ACP agent.",
            &["agent", "acp"],
        ),
        spec(
            "new-session",
            "New Session…",
            "Terminal",
            Some("cmd+shift+n"),
            "Create a named session.",
            &[],
        ),
        spec(
            "rename-session",
            "Rename Session…",
            "Terminal",
            None,
            "Change the current session's name.",
            &[],
        ),
        spec(
            "select-session-work",
            "Select All in Session",
            "Organization",
            None,
            "Select the current session's work for organization.",
            &[],
        ),
        spec(
            "move-resources",
            "Move to Session…",
            "Organization",
            Some("cmd+shift+m"),
            "Choose a session for the selected work.",
            &[],
        ),
        spec(
            "undo-organization",
            "Undo Organization",
            "Organization",
            Some("cmd+alt+z"),
            "Reverse the last organization change.",
            &[],
        ),
        spec(
            "organize-everything",
            "Organize Everything",
            "Organization",
            Some("cmd+shift+o"),
            "Move every pane into the session for its repository or task.",
            &["tidy", "sort", "group"],
        ),
        spec(
            "undo-organize-everything",
            "Undo Organize Everything",
            "Organization",
            None,
            "Reverse the last Organize Everything run.",
            &[],
        ),
        spec(
            "kill-session",
            "Kill Session…",
            "Terminal",
            None,
            "Confirm termination of a session and its processes.",
            &["delete"],
        ),
        spec(
            "new-home",
            "New Terminal in Home Directory",
            "Terminal",
            Some("cmd+shift+t"),
            "Open a new terminal starting in your home directory.",
            &["shell"],
        ),
        spec(
            "folder",
            "Open Folder…",
            "Terminal",
            Some("cmd+o"),
            "Choose a folder for a new terminal.",
            &["directory", "project"],
        ),
        spec(
            "insert-path",
            "Insert Path…",
            "Terminal",
            Some("cmd+shift+i"),
            "Choose a file or folder path to insert into the terminal.",
            &[],
        ),
        spec(
            "split-right",
            "Split Right",
            "Panes",
            Some("cmd+d"),
            "Open a terminal beside the focused pane.",
            &["vertical"],
        ),
        spec(
            "split-down",
            "Split Down",
            "Panes",
            Some("cmd+shift+d"),
            "Open a terminal below the focused pane.",
            &["horizontal"],
        ),
        spec(
            "split-left",
            "Split Left",
            "Panes",
            None,
            "Open a terminal to the left of the focused pane.",
            &[],
        ),
        spec(
            "split-up",
            "Split Up",
            "Panes",
            None,
            "Open a terminal above the focused pane.",
            &[],
        ),
        spec(
            "view",
            "Mirror Pane",
            "Panes",
            Some("cmd+alt+d"),
            "Show the focused terminal in another pane.",
            &["duplicate"],
        ),
        spec(
            "close",
            "Close Pane",
            "Panes",
            Some("cmd+w"),
            "Detach this pane; its terminal keeps running.",
            &[],
        ),
        spec(
            "close-tab",
            "Close Tab",
            "Tabs",
            Some("cmd+shift+w"),
            "Detach this tab; its terminals keep running.",
            &[],
        ),
        spec(
            "zoom",
            "Zoom Pane",
            "Panes",
            Some("cmd+shift+enter"),
            "Expand the focused pane or restore its splits.",
            &[],
        ),
        spec(
            "equalize",
            "Equalize Splits",
            "Panes",
            Some("cmd+ctrl+="),
            "Give the active tab's panes equal space.",
            &["balance"],
        ),
        spec(
            "pane-left",
            "Focus Pane Left",
            "Panes",
            Some("cmd+alt+left"),
            "Focus the neighboring pane on the left.",
            &[],
        ),
        spec(
            "pane-right",
            "Focus Pane Right",
            "Panes",
            Some("cmd+alt+right"),
            "Focus the neighboring pane on the right.",
            &[],
        ),
        spec(
            "pane-up",
            "Focus Pane Up",
            "Panes",
            Some("cmd+alt+up"),
            "Focus the neighboring pane above.",
            &[],
        ),
        spec(
            "pane-down",
            "Focus Pane Down",
            "Panes",
            Some("cmd+alt+down"),
            "Focus the neighboring pane below.",
            &[],
        ),
        spec(
            "pane-next",
            "Next Pane",
            "Panes",
            Some("cmd+]"),
            "Focus the next pane in the active tab.",
            &[],
        ),
        spec(
            "pane-prev",
            "Previous Pane",
            "Panes",
            Some("cmd+["),
            "Focus the previous pane in the active tab.",
            &[],
        ),
        spec(
            "tab-next",
            "Next Tab",
            "Tabs",
            Some("cmd+shift+]"),
            "Switch to the next tab.",
            &[],
        ),
        spec(
            "tab-prev",
            "Previous Tab",
            "Tabs",
            Some("cmd+shift+["),
            "Switch to the previous tab.",
            &[],
        ),
        spec(
            "tab-move-left",
            "Move Tab Left",
            "Tabs",
            None,
            "Move the active tab earlier in the tab bar.",
            &["reorder"],
        ),
        spec(
            "tab-move-right",
            "Move Tab Right",
            "Tabs",
            None,
            "Move the active tab later in the tab bar.",
            &["reorder"],
        ),
        spec(
            "rename",
            "Rename Tab…",
            "Tabs",
            None,
            "Set a custom name for the active tab.",
            &[],
        ),
        spec(
            "window",
            "Move Pane to New Window",
            "Panes",
            Some("cmd+alt+n"),
            "Move the focused terminal into a separate window.",
            &["detach"],
        ),
        spec(
            "window-next",
            "Move Pane to Next Window",
            "Panes",
            None,
            "Move the focused terminal into the next open window on the same server.",
            &["window", "transfer"],
        ),
        spec(
            "find",
            "Find in Terminal",
            "Terminal",
            Some("cmd+f"),
            "Search the focused terminal's output.",
            &["search", "scrollback"],
        ),
        spec(
            "find-next",
            "Find Next",
            "Terminal",
            Some("cmd+g"),
            "Select the next search match.",
            &[],
        ),
        spec(
            "find-prev",
            "Find Previous",
            "Terminal",
            Some("cmd+shift+g"),
            "Select the previous search match.",
            &[],
        ),
        spec(
            "find-selection",
            "Use Selection for Find",
            "Terminal",
            Some("cmd+e"),
            "Search for the selected terminal text.",
            &[],
        ),
        spec(
            "find-close",
            "Close Find",
            "Terminal",
            None,
            "Dismiss the terminal search bar.",
            &[],
        ),
        spec(
            "follow",
            "Scroll to Live Output",
            "Terminal",
            Some("cmd+l"),
            "Return to the latest terminal output.",
            &["bottom"],
        ),
        spec(
            "paste-cwd",
            "Paste Working Directory",
            "Terminal",
            None,
            "Insert the focused terminal's working directory.",
            &["cwd", "path"],
        ),
        spec(
            "terminate",
            "Terminate Terminal Process…",
            "Terminal",
            None,
            "Confirm termination of the focused terminal process.",
            &["kill"],
        ),
        spec(
            "attention",
            "Jump to Agent Needing Attention",
            "Agents",
            Some("cmd+shift+a"),
            "Focus the next agent that needs your attention.",
            &[],
        ),
        spec(
            "sidebar",
            "Toggle Sidebar",
            "View",
            Some("cmd+b"),
            "Show or hide the navigation sidebar.",
            &[],
        ),
        spec(
            "plain-terminal",
            "Toggle Plain Terminal",
            "View",
            Some("cmd+ctrl+t"),
            "Switch between the glass desk and a plain terminal window.",
            &["skin", "minimal", "ghostty", "chrome", "glass"],
        ),
        spec(
            "tabs-top",
            "Tabs on Top",
            "View",
            None,
            "Place workspace tabs in the titlebar.",
            &["layout", "horizontal", "placement"],
        ),
        spec(
            "tabs-side",
            "Tabs on Side",
            "View",
            None,
            "Place workspace tabs in a scrollable left rail.",
            &["layout", "vertical", "placement"],
        ),
        CommandSpec {
            aliases: &["cmd+shift+="],
            ..spec(
                "font-up",
                "Increase Font Size",
                "View",
                Some("cmd+="),
                "Make terminal text larger.",
                &[],
            )
        },
        spec(
            "font-down",
            "Decrease Font Size",
            "View",
            Some("cmd+-"),
            "Make terminal text smaller.",
            &[],
        ),
        spec(
            "font-reset",
            "Reset Font Size",
            "View",
            Some("cmd+0"),
            "Restore the default terminal text size.",
            &[],
        ),
        spec(
            "notifications",
            "Show Inbox & Notifications",
            "View",
            None,
            "Review inbox entries and desktop notices.",
            &["alerts"],
        ),
        spec(
            "clear-notifications",
            "Clear Inbox & Notifications",
            "View",
            None,
            "Remove all entries from the notification feed.",
            &[],
        ),
        spec(
            "resize-left",
            "Move Divider Left",
            "Panes",
            None,
            "Nudge the focused pane's divider to the left.",
            &[],
        ),
        spec(
            "resize-right",
            "Move Divider Right",
            "Panes",
            None,
            "Nudge the focused pane's divider to the right.",
            &[],
        ),
        spec(
            "resize-up",
            "Move Divider Up",
            "Panes",
            None,
            "Nudge the focused pane's divider upward.",
            &[],
        ),
        spec(
            "resize-down",
            "Move Divider Down",
            "Panes",
            None,
            "Nudge the focused pane's divider downward.",
            &[],
        ),
        spec(
            "scroll-page-up",
            "Scroll Page Up",
            "Terminal",
            Some("cmd+pageup"),
            "Scroll terminal output up by one page.",
            &[],
        ),
        spec(
            "scroll-page-down",
            "Scroll Page Down",
            "Terminal",
            Some("cmd+pagedown"),
            "Scroll terminal output down by one page.",
            &[],
        ),
        spec(
            "scroll-top",
            "Scroll to Top",
            "Terminal",
            Some("cmd+home"),
            "Scroll to the oldest available terminal output.",
            &[],
        ),
        spec(
            "scroll-bottom",
            "Scroll to Bottom",
            "Terminal",
            Some("cmd+end"),
            "Return to the latest terminal output.",
            &[],
        ),
        spec(
            "clear",
            "Clear Screen",
            "Terminal",
            Some("cmd+k"),
            "Send the clear-screen control to the terminal.",
            &[],
        ),
        spec(
            "select-all",
            "Select All",
            "Terminal",
            Some("cmd+a"),
            "Select all available terminal text.",
            &[],
        ),
        spec(
            "copy",
            "Copy",
            "Terminal",
            None,
            "Copy the terminal selection to the clipboard.",
            &[],
        ),
        spec(
            "paste",
            "Paste",
            "Terminal",
            None,
            "Insert clipboard text into the focused terminal.",
            &[],
        ),
        spec(
            "paste-selection",
            "Paste Selection",
            "Terminal",
            None,
            "Insert the selected terminal text as input.",
            &[],
        ),
        spec(
            "prompt-prev",
            "Jump to Previous Prompt",
            "Terminal",
            Some("cmd+up"),
            "Scroll to the previous shell prompt marker.",
            &[],
        ),
        spec(
            "prompt-next",
            "Jump to Next Prompt",
            "Terminal",
            Some("cmd+down"),
            "Scroll to the next shell prompt marker.",
            &[],
        ),
        spec(
            "write-file:scrollback:open",
            "Open Scrollback as a File",
            "Terminal",
            None,
            "Write terminal scrollback to a file and open it.",
            &["export", "editor"],
        ),
        spec(
            "quick-terminal",
            "Toggle Quick Terminal",
            "View",
            None,
            "Open or close the quick-terminal window.",
            &["dropdown", "hotkey"],
        ),
        spec(
            "new-window",
            "New Window",
            "View",
            Some("cmd+n"),
            "Open another window on a session.",
            &[],
        ),
        spec(
            "fullscreen",
            "Toggle Full Screen",
            "View",
            Some("cmd+ctrl+f"),
            "Enter or leave native full-screen mode.",
            &[],
        ),
        spec(
            "maximize",
            "Maximize Window",
            "View",
            None,
            "Toggle the native window's zoomed size.",
            &[],
        ),
        spec(
            "reload-config",
            "Reload Ghostty Config",
            "Ghostty",
            Some("cmd+shift+,"),
            "Read the Ghostty configuration again.",
            &[],
        ),
        spec(
            "import-ghostty",
            "Use Ghostty Font, Colors and Padding",
            "Ghostty",
            None,
            "Apply Ghostty appearance settings to this window.",
            &[],
        ),
        spec(
            "reconnect",
            "Reconnect to Server",
            "Connection",
            Some("cmd+shift+r"),
            "Restart this window's connection to the server.",
            &[],
        ),
        spec(
            "performance-monitor",
            "Performance Monitor",
            "Utilities",
            None,
            "Inspect desktop CPU, memory and render timings.",
            &["telemetry", "cpu", "memory", "rss"],
        ),
        spec(
            "performance-monitor-stop",
            "Stop Performance Monitoring",
            "Utilities",
            None,
            "Close the performance window and stop its sampler.",
            &["telemetry", "cpu", "memory", "rss"],
        ),
    ];
    for (id, chord) in [
        ("tab-1", "cmd+1"),
        ("tab-2", "cmd+2"),
        ("tab-3", "cmd+3"),
        ("tab-4", "cmd+4"),
        ("tab-5", "cmd+5"),
        ("tab-6", "cmd+6"),
        ("tab-7", "cmd+7"),
        ("tab-8", "cmd+8"),
        ("tab-9", "cmd+9"),
    ] {
        commands.push(spec(
            id,
            if id == "tab-9" {
                "Go to Last Tab"
            } else {
                "Go to Tab"
            },
            "Tabs",
            Some(chord),
            if id == "tab-9" {
                "Switch to the last tab."
            } else {
                "Switch to the tab at this position."
            },
            &[],
        ));
    }
    commands
}

fn spec(
    id: &'static str,
    title: &'static str,
    group: &'static str,
    chord: Option<&'static str>,
    description: &'static str,
    keywords: &'static [&'static str],
) -> CommandSpec {
    CommandSpec {
        id,
        title,
        description,
        keywords,
        group,
        chord,
        aliases: &[],
    }
}

/// Built-in chord to command id, primary chord before aliases.
#[must_use]
pub fn default_chords() -> Vec<(String, String)> {
    let mut chords = Vec::new();
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
    let (key, lowered, raw_shifted) = normalize_key(&event.key);
    let shift = event.shift || key != lowered || raw_shifted;
    join_chord(true, event.ctrl, event.alt, shift, &key)
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
    let (key, lowered, raw_shifted) = normalize_key(&event.key);
    let shift = event.shift || key != lowered || raw_shifted;
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

/// macOS modifier order: control, option, shift, command, then the key.
#[must_use]
pub fn display_chord(chord: &str) -> String {
    let parts: Vec<&str> = chord.split('+').collect();
    let key = parts.last().copied().unwrap_or("");
    let glyphs = HashMap::from([
        ("cmd", "⌘"),
        ("ctrl", "⌃"),
        ("alt", "⌥"),
        ("shift", "⇧"),
        ("enter", "↩"),
        ("up", "↑"),
        ("down", "↓"),
        ("left", "←"),
        ("right", "→"),
        ("escape", "esc"),
        ("backspace", "⌫"),
    ]);
    let mut shown = String::new();
    for modifier in ["ctrl", "alt", "shift", "cmd"] {
        if parts.contains(&modifier) {
            shown.push_str(glyphs.get(modifier).copied().unwrap_or(modifier));
        }
    }
    let label = glyphs
        .get(key)
        .copied()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if key.chars().count() == 1 {
                key.to_uppercase()
            } else {
                key.to_owned()
            }
        });
    shown.push_str(&label);
    shown
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

fn normalize_key(raw: &str) -> (String, String, bool) {
    let lowered = raw.to_lowercase();
    let key = UNSHIFTED
        .iter()
        .find(|(shifted, _)| *shifted == lowered)
        .map(|(_, plain)| (*plain).to_owned())
        .unwrap_or_else(|| lowered.clone());
    let shifted = lowered != raw;
    (key, lowered, shifted)
}

fn join_chord(cmd: bool, ctrl: bool, alt: bool, shift: bool, key: &str) -> String {
    let mut parts = Vec::new();
    if cmd {
        parts.push("cmd");
    }
    if ctrl {
        parts.push("ctrl");
    }
    if alt {
        parts.push("alt");
    }
    if shift {
        parts.push("shift");
    }
    parts.push(key);
    parts.join("+")
}
