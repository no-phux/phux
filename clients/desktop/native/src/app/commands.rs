//! Command catalog. Chords come from `settings`; this list is the titles and groups.

/// One palette and menu command.
#[derive(Clone, Copy, Debug)]
pub struct CommandSpec {
    /// Stable id shared with Ghostty action mapping and key bindings.
    pub id: &'static str,
    /// Palette and menu title.
    pub title: &'static str,
    /// Palette section.
    pub group: &'static str,
    /// Built-in chord in `cmd+shift+p` form. Ghostty may replace it.
    pub chord: Option<&'static str>,
    /// Extra built-in chords, such as `cmd+shift+=` beside `cmd+=`.
    pub aliases: &'static [&'static str],
}

/// Every command the palette lists, in registry order.
pub const COMMANDS: &[CommandSpec] = &[
    spec("palette", "Command Palette", "General", Some("cmd+shift+p")),
    spec("goto", "Go to Terminal", "General", Some("cmd+p")),
    spec("settings", "Settings", "General", Some("cmd+,")),
    spec("new", "New Terminal Tab", "Terminal", Some("cmd+t")),
    spec("new-session", "New Session…", "Terminal", Some("cmd+shift+n")),
    spec("rename-session", "Rename Session…", "Terminal", None),
    spec("new-home", "New Terminal in Home Directory", "Terminal", Some("cmd+shift+t")),
    spec("folder", "Open Folder…", "Terminal", Some("cmd+o")),
    spec("insert-path", "Insert Path…", "Terminal", Some("cmd+shift+i")),
    spec("split-right", "Split Right", "Panes", Some("cmd+d")),
    spec("split-down", "Split Down", "Panes", Some("cmd+shift+d")),
    spec("split-left", "Split Left", "Panes", None),
    spec("split-up", "Split Up", "Panes", None),
    spec("view", "Open Another View of This Terminal", "Panes", Some("cmd+alt+d")),
    spec("close", "Close Pane", "Panes", Some("cmd+w")),
    spec("close-tab", "Close Tab", "Tabs", Some("cmd+shift+w")),
    spec("zoom", "Zoom Pane", "Panes", Some("cmd+shift+enter")),
    spec("equalize", "Equalize Splits", "Panes", Some("cmd+ctrl+=")),
    spec("pane-left", "Focus Pane Left", "Panes", Some("cmd+alt+left")),
    spec("pane-right", "Focus Pane Right", "Panes", Some("cmd+alt+right")),
    spec("pane-up", "Focus Pane Up", "Panes", Some("cmd+alt+up")),
    spec("pane-down", "Focus Pane Down", "Panes", Some("cmd+alt+down")),
    spec("pane-next", "Next Pane", "Panes", Some("cmd+]")),
    spec("pane-prev", "Previous Pane", "Panes", Some("cmd+[")),
    spec("tab-next", "Next Tab", "Tabs", Some("cmd+shift+]")),
    spec("tab-prev", "Previous Tab", "Tabs", Some("cmd+shift+[")),
    spec("tab-move-left", "Move Tab Left", "Tabs", None),
    spec("tab-move-right", "Move Tab Right", "Tabs", None),
    spec("rename", "Rename Tab…", "Tabs", None),
    spec("window", "Move Pane to New Window", "Panes", Some("cmd+alt+n")),
    spec("find", "Find in Terminal", "Terminal", Some("cmd+f")),
    spec("find-next", "Find Next", "Terminal", Some("cmd+g")),
    spec("find-prev", "Find Previous", "Terminal", Some("cmd+shift+g")),
    spec("find-selection", "Use Selection for Find", "Terminal", Some("cmd+e")),
    spec("find-close", "Close Find", "Terminal", None),
    spec("follow", "Scroll to Live Output", "Terminal", Some("cmd+l")),
    spec("paste-cwd", "Paste Working Directory", "Terminal", None),
    spec("terminate", "Terminate Terminal Process…", "Terminal", None),
    spec("attention", "Jump to Agent Needing Attention", "Agents", Some("cmd+shift+a")),
    spec("sidebar", "Toggle Sidebar", "View", Some("cmd+b")),
    spec("font-up", "Increase Font Size", "View", Some("cmd+=")),
    spec("font-down", "Decrease Font Size", "View", Some("cmd+-")),
    spec("font-reset", "Reset Font Size", "View", Some("cmd+0")),
    spec("option-alt", "Toggle Option as Alt", "View", None),
    spec("notify", "Toggle Agent Notifications", "Agents", None),
    spec("scroll-page-up", "Scroll Page Up", "Terminal", Some("cmd+pageup")),
    spec("scroll-page-down", "Scroll Page Down", "Terminal", Some("cmd+pagedown")),
    spec("scroll-top", "Scroll to Top", "Terminal", Some("cmd+home")),
    spec("scroll-bottom", "Scroll to Bottom", "Terminal", Some("cmd+end")),
    spec("clear", "Clear Screen", "Terminal", Some("cmd+k")),
    spec("select-all", "Select All", "Terminal", Some("cmd+a")),
    spec("copy", "Copy", "Terminal", None),
    spec("paste", "Paste", "Terminal", None),
    spec("paste-selection", "Paste Selection", "Terminal", None),
    spec("prompt-prev", "Jump to Previous Prompt", "Terminal", Some("cmd+up")),
    spec("prompt-next", "Jump to Next Prompt", "Terminal", Some("cmd+down")),
    spec("write-screen-copy", "Copy Screen and Scrollback as a File", "Terminal", None),
    spec("write-screen-paste", "Paste Screen and Scrollback as a File", "Terminal", None),
    spec("write-screen-open", "Open Screen and Scrollback as a File", "Terminal", None),
    spec("write-scrollback-copy", "Copy Scrollback as a File", "Terminal", None),
    spec("write-scrollback-paste", "Paste Scrollback as a File", "Terminal", None),
    spec("write-scrollback-open", "Open Scrollback as a File", "Terminal", None),
    spec("write-selection-copy", "Copy Selection as a File", "Terminal", None),
    spec("write-selection-paste", "Paste Selection as a File", "Terminal", None),
    spec("write-selection-open", "Open Selection as a File", "Terminal", None),
    spec("quick-terminal", "Toggle Quick Terminal", "View", None),
    spec("new-window", "New Window", "View", Some("cmd+n")),
    spec("fullscreen", "Toggle Full Screen", "View", Some("cmd+ctrl+f")),
    spec("maximize", "Zoom Window", "View", None),
    spec("reload-config", "Reload Ghostty Config", "Ghostty", Some("cmd+shift+,")),
    spec("import-ghostty", "Use Ghostty Font, Colors and Padding", "Ghostty", None),
    spec("reconnect", "Reconnect to Server", "Connection", Some("cmd+shift+r")),
    spec("tab-1", "Select Tab 1", "Tabs", Some("cmd+1")),
    spec("tab-2", "Select Tab 2", "Tabs", Some("cmd+2")),
    spec("tab-3", "Select Tab 3", "Tabs", Some("cmd+3")),
    spec("tab-4", "Select Tab 4", "Tabs", Some("cmd+4")),
    spec("tab-5", "Select Tab 5", "Tabs", Some("cmd+5")),
    spec("tab-6", "Select Tab 6", "Tabs", Some("cmd+6")),
    spec("tab-7", "Select Tab 7", "Tabs", Some("cmd+7")),
    spec("tab-8", "Select Tab 8", "Tabs", Some("cmd+8")),
    spec("tab-9", "Select Last Tab", "Tabs", Some("cmd+9")),
    spec("resize-left", "Resize Split Left", "Panes", None),
    spec("resize-right", "Resize Split Right", "Panes", None),
    spec("resize-up", "Resize Split Up", "Panes", None),
    spec("resize-down", "Resize Split Down", "Panes", None),
];

const FONT_UP_ALIASES: &[&str] = &["cmd+shift+="];

const fn spec(
    id: &'static str,
    title: &'static str,
    group: &'static str,
    chord: Option<&'static str>,
) -> CommandSpec {
    CommandSpec { id, title, group, chord, aliases: &[] }
}

pub fn builtin_chords() -> Vec<(String, &'static str)> {
    let mut chords = Vec::new();
    for command in COMMANDS {
        if let Some(chord) = command.chord {
            chords.push((chord.to_owned(), command.id));
        }
        let aliases = if command.id == "font-up" { FONT_UP_ALIASES } else { command.aliases };
        for alias in aliases {
            chords.push(((*alias).to_owned(), command.id));
        }
    }
    chords
}

pub fn find(id: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|command| command.id == id)
}

pub fn title_of(id: &str) -> String {
    find(id).map(|command| command.title.to_owned()).unwrap_or_else(|| id.to_owned())
}

/// Palette rows that are not numbered tab jumps.
pub fn palette_commands() -> impl Iterator<Item = &'static CommandSpec> {
    COMMANDS.iter().filter(|command| !command.id.starts_with("tab-") || command.id == "tab-next" || command.id == "tab-prev" || command.id.starts_with("tab-move"))
}
