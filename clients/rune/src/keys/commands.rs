//! The one table of what the app can do: each command's title and the chords
//! that reach it on macOS and elsewhere. Key handling and the palette read
//! it, so a chord and what it says beside a command can't disagree.

use super::Shortcut;

mod keycaps;

/// The key of a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Char(char),
    Enter,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
}

impl Cap {
    fn accelerator_name(self) -> String {
        match self {
            Self::Char(c) => c.to_string(),
            Self::Enter => "enter".to_owned(),
            Self::Left => "left".to_owned(),
            Self::Right => "right".to_owned(),
            Self::Up => "up".to_owned(),
            Self::Down => "down".to_owned(),
            Self::Home => "home".to_owned(),
            Self::End => "end".to_owned(),
            Self::PageUp => "pageup".to_owned(),
            Self::PageDown => "pagedown".to_owned(),
            Self::Insert => "insert".to_owned(),
        }
    }

    fn mac_name(self) -> String {
        match self {
            Self::Char(c) => c.to_ascii_uppercase().to_string(),
            Self::Enter => "↩".to_owned(),
            Self::Left => "←".to_owned(),
            Self::Right => "→".to_owned(),
            Self::Up => "↑".to_owned(),
            Self::Down => "↓".to_owned(),
            Self::Home => "↖".to_owned(),
            Self::End => "↘".to_owned(),
            Self::PageUp => "⇞".to_owned(),
            Self::PageDown => "⇟".to_owned(),
            Self::Insert => "Insert".to_owned(),
        }
    }

    fn linux_name(self) -> String {
        match self {
            Self::Enter => "Enter".to_owned(),
            Self::Home => "Home".to_owned(),
            Self::End => "End".to_owned(),
            Self::PageUp => "PgUp".to_owned(),
            Self::PageDown => "PgDn".to_owned(),
            other => other.mac_name(),
        }
    }
}

/// A key and exactly the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub cap: Cap,
    /// ⌘ on macOS, Super elsewhere.
    pub command: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Chord {
    const fn cmd(cap: Cap) -> Self {
        Self {
            cap,
            command: true,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    const fn ctrl(cap: Cap) -> Self {
        Self {
            cap,
            command: false,
            ctrl: true,
            alt: false,
            shift: false,
        }
    }

    const fn shift(self) -> Self {
        Self {
            shift: true,
            ..self
        }
    }

    /// The key with Shift alone.
    const fn shifted(cap: Cap) -> Self {
        Self {
            cap,
            command: false,
            ctrl: false,
            alt: false,
            shift: true,
        }
    }

    const fn alt(self) -> Self {
        Self { alt: true, ..self }
    }

    /// The key with Option (Alt) alone.
    const fn opt(cap: Cap) -> Self {
        Self {
            cap,
            command: false,
            ctrl: false,
            alt: true,
            shift: false,
        }
    }

    const fn and_ctrl(self) -> Self {
        Self { ctrl: true, ..self }
    }

    /// The accelerator Rune parses for a native menu: `cmd+shift+d`.
    pub fn accelerator(self) -> String {
        let mods = [
            (self.command, "cmd"),
            (self.ctrl, "ctrl"),
            (self.alt, "alt"),
            (self.shift, "shift"),
        ];
        let held = mods.iter().filter(|(held, _)| *held).map(|(_, name)| *name);
        let key = self.cap.accelerator_name();
        held.chain([key.as_str()]).collect::<Vec<_>>().join("+")
    }

    /// As this platform writes it.
    fn label(self) -> String {
        if cfg!(any(target_os = "macos", target_os = "ios")) {
            self.mac_label()
        } else {
            self.linux_label()
        }
    }

    /// As macOS writes it: `⇧⌘D`.
    fn mac_label(self) -> String {
        let marks = [
            (self.ctrl, "⌃"),
            (self.alt, "⌥"),
            (self.shift, "⇧"),
            (self.command, "⌘"),
        ];
        let held = marks
            .iter()
            .filter(|(held, _)| *held)
            .map(|(_, mark)| *mark);
        held.chain([self.cap.mac_name().as_str()]).collect()
    }

    /// As Linux desktops write it: `Ctrl+Shift+D`.
    fn linux_label(self) -> String {
        let marks = [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.command, "Super"),
        ];
        let held = marks
            .iter()
            .filter(|(held, _)| *held)
            .map(|(_, mark)| *mark);
        let name = self.cap.linux_name();
        held.chain([name.as_str()]).collect::<Vec<_>>().join("+")
    }
}

/// One thing the app can do.
#[derive(Debug)]
pub struct Command {
    pub action: Shortcut,
    pub title: &'static str,
    /// Its chords, the first shown beside it; aliases follow.
    pub mac: &'static [Chord],
    pub linux: &'static [Chord],
    /// Whether the palette offers it.
    pub listed: bool,
}

impl Command {
    /// This platform's chords.
    pub const fn chords(&self) -> &'static [Chord] {
        if cfg!(any(target_os = "macos", target_os = "ios")) {
            self.mac
        } else {
            self.linux
        }
    }

    /// Every chord that reaches it, for the shortcut list.
    pub fn hints(&self) -> String {
        let chords = self.chords().iter();
        chords
            .map(|chord| chord.label())
            .collect::<Vec<_>>()
            .join("  ")
    }

    /// The chord shown beside it, or nothing.
    pub fn hint(&self) -> String {
        self.chords()
            .first()
            .map_or_else(String::new, |c| c.label())
    }
}

const fn m(c: char) -> Chord {
    Chord::cmd(Cap::Char(c))
}

const fn l(c: char) -> Chord {
    Chord::ctrl(Cap::Char(c)).shift()
}

/// A row: action, title, macOS chords, other chords. A struct literal, so
/// the chord slices live as long as the table.
macro_rules! row {
    ($action:expr, $title:expr, [$($mac:expr),*], [$($linux:expr),*]) => {
        Command {
            action: $action,
            title: $title,
            mac: &[$($mac),*],
            linux: &[$($linux),*],
            listed: true,
        }
    };
    (mac $action:expr, $title:expr, [$($mac:expr),*]) => {
        Command {
            action: $action,
            title: $title,
            mac: &[$($mac),*],
            linux: &[],
            listed: cfg!(any(target_os = "macos", target_os = "ios")),
        }
    };
    (hidden $action:expr, $title:expr, [$($mac:expr),*], [$($linux:expr),*]) => {
        Command {
            action: $action,
            title: $title,
            mac: &[$($mac),*],
            linux: &[$($linux),*],
            listed: false,
        }
    };
}

/// A tab chord's row.
macro_rules! tab {
    ($index:expr, $digit:expr, $title:expr) => {
        row!(hidden Shortcut::Tab($index), $title, [m($digit)], [l($digit)])
    };
}

pub const COMMANDS: &[Command] = &[
    row!(Shortcut::NewTerminal, "New Tab", [m('t')], [l('t')]),
    row!(Shortcut::NewSession, "New Session", [m('n')], [l('n')]),
    row!(
        Shortcut::JumpToAttention,
        "Jump to What Needs You",
        [m('j'), m('a').shift()],
        [l('j')]
    ),
    row!(Shortcut::Inbox, "Open Inbox", [m('i')], [l('i')]),
    row!(Shortcut::CopyConfig, "Copy Settings as TOML", [], []),
    row!(Shortcut::Galaxy, "Open Galaxy", [], []),
    row!(Shortcut::Reef, "Open Reef", [], []),
    row!(Shortcut::Pitch, "Open Pitch", [], []),
    row!(Shortcut::Kitchen, "Open Kitchen", [], []),
    row!(Shortcut::PitLane, "Open Pit Lane", [], []),
    row!(Shortcut::Hive, "Open Hive", [], []),
    row!(Shortcut::Settings, "Settings", [m(',')], [l(',')]),
    row!(Shortcut::ChangeTheme, "Change Theme", [], []),
    row!(
        Shortcut::SwitchSession,
        "Switch Session",
        [m('s').shift()],
        [l('s')]
    ),
    row!(
        Shortcut::InsertPath,
        "Insert Path",
        [m('i').shift(), Chord::opt(Cap::Char(';'))],
        [l(';')]
    ),
    row!(Shortcut::KeyboardShortcuts, "Keyboard Shortcuts", [], []),
    row!(
        Shortcut::EssentialShortcuts,
        "Keyboard Shortcuts: Essentials",
        [],
        []
    ),
    row!(Shortcut::GetStarted, "Get Started", [], []),
    row!(Shortcut::CheckForUpdates, "Check for Updates", [], []),
    row!(Shortcut::InstallUpdate, "Update Now", [], []),
    row!(Shortcut::Machines, "Switch or Add Machine", [], []),
    row!(Shortcut::RetryConnection, "Retry Connection", [], []),
    row!(Shortcut::CopyDiagnostics, "Copy Diagnostics", [], []),
    row!(Shortcut::RenameSession, "Rename Session", [], []),
    row!(Shortcut::RenameTab, "Rename Tab", [], []),
    row!(Shortcut::CloseSession, "Close Session", [], []),
    row!(Shortcut::SplitRight, "Split Right", [m('d')], [l('d')]),
    row!(
        Shortcut::SplitDown,
        "Split Down",
        [m('d').shift()],
        [l('e')]
    ),
    row!(
        Shortcut::ZoomPane,
        "Zoom Pane",
        [Chord::cmd(Cap::Enter).shift()],
        [Chord::ctrl(Cap::Enter).shift()]
    ),
    row!(
        Shortcut::CloseTerminal,
        "Close Terminal",
        [m('w')],
        [l('w')]
    ),
    row!(
        Shortcut::CloseTab,
        "Close Tab",
        [m('w').shift()],
        [l('w').alt()]
    ),
    row!(Shortcut::Find, "Find in Terminal", [m('f')], [l('f')]),
    row!(Shortcut::CopyMode, "Copy Mode", [m('c').shift()], [l('x')]),
    row!(
        Shortcut::FindSelection,
        "Find Selected Text",
        [m('e')],
        [l('e').alt()]
    ),
    row!(
        Shortcut::FocusLeft,
        "Focus Pane Left",
        [Chord::cmd(Cap::Left).alt()],
        [Chord::ctrl(Cap::Left).alt()]
    ),
    row!(
        Shortcut::FocusRight,
        "Focus Pane Right",
        [Chord::cmd(Cap::Right).alt()],
        [Chord::ctrl(Cap::Right).alt()]
    ),
    row!(
        Shortcut::FocusUp,
        "Focus Pane Up",
        [Chord::cmd(Cap::Up).alt()],
        [Chord::ctrl(Cap::Up).alt()]
    ),
    row!(
        Shortcut::FocusDown,
        "Focus Pane Down",
        [Chord::cmd(Cap::Down).alt()],
        [Chord::ctrl(Cap::Down).alt()]
    ),
    row!(Shortcut::NextTerminal, "Next Terminal", [m(']')], [l(']')]),
    row!(
        Shortcut::PreviousTerminal,
        "Previous Terminal",
        [m('[')],
        [l('[')]
    ),
    row!(
        Shortcut::NextTab,
        "Next Tab",
        [m(']').shift()],
        [Chord::ctrl(Cap::PageDown).shift()]
    ),
    row!(
        Shortcut::PreviousTab,
        "Previous Tab",
        [m('[').shift()],
        [Chord::ctrl(Cap::PageUp).shift()]
    ),
    row!(
        Shortcut::ToggleSidebar,
        "Toggle Sidebar",
        [m('b')],
        [l('b')]
    ),
    row!(mac Shortcut::ToggleFullScreen, "Toggle Full Screen", [m('f').and_ctrl()]),
    // Ctrl+Insert and Shift+Insert are what Omarchy's Super+C and Super+V
    // send a window tagged a terminal.
    row!(
        Shortcut::Copy,
        "Copy Selection",
        [m('c')],
        [l('c'), Chord::ctrl(Cap::Insert)]
    ),
    row!(
        Shortcut::Paste,
        "Paste",
        [m('v')],
        [l('v'), Chord::shifted(Cap::Insert)]
    ),
    row!(Shortcut::SelectAll, "Select All", [m('a')], [l('a')]),
    // ⌘+ is Shift and the = key on most layouts.
    row!(
        Shortcut::ZoomIn,
        "Bigger Text",
        [m('='), m('=').shift()],
        [l('=')]
    ),
    row!(Shortcut::ZoomOut, "Smaller Text", [m('-')], [l('-')]),
    row!(Shortcut::ZoomReset, "Default Text Size", [m('0')], [l('0')]),
    row!(
        Shortcut::ScrollTop,
        "Scroll to Top of History",
        [Chord::cmd(Cap::Home)],
        [Chord::ctrl(Cap::Home).shift()]
    ),
    row!(
        Shortcut::ScrollBottom,
        "Scroll to Bottom",
        [m('l'), Chord::cmd(Cap::End)],
        [l('l'), Chord::ctrl(Cap::End).shift()]
    ),
    row!(
        Shortcut::PromptUp,
        "Jump to Previous Prompt",
        [Chord::cmd(Cap::Up)],
        [Chord::ctrl(Cap::Up).shift()]
    ),
    row!(
        Shortcut::PromptDown,
        "Jump to Next Prompt",
        [Chord::cmd(Cap::Down)],
        [Chord::ctrl(Cap::Down).shift()]
    ),
    row!(hidden Shortcut::Palette, "Command Palette", [m('p'), m('k'), m('p').shift()], [l('p'), l('k')]),
    row!(hidden Shortcut::FindNext, "Find Next", [m('g')], [l('g')]),
    row!(hidden Shortcut::FindPrevious, "Find Previous", [m('g').shift()], [l('g').alt()]),
    // Unlisted anywhere: found, not taught.
    row!(hidden Shortcut::Bare, "Just the Terminal UI", [m('t').shift().alt()], [l('t').alt()]),
    tab!(0, '1', "Tab 1"),
    tab!(1, '2', "Tab 2"),
    tab!(2, '3', "Tab 3"),
    tab!(3, '4', "Tab 4"),
    tab!(4, '5', "Tab 5"),
    tab!(5, '6', "Tab 6"),
    tab!(6, '7', "Tab 7"),
    tab!(7, '8', "Tab 8"),
    tab!(8, '9', "Tab 9"),
];

/// The table's row for `action`.
/// The keys worth learning first, in teaching order: the ones that make a
/// terminal, move between terminals, and find what needs you. Every one has
/// a chord on each platform (tested), so the list never shows a bare title.
pub const ESSENTIALS: &[Shortcut] = &[
    Shortcut::NewTerminal,
    Shortcut::NewSession,
    Shortcut::Palette,
    Shortcut::SwitchSession,
    Shortcut::JumpToAttention,
    Shortcut::Inbox,
    Shortcut::SplitRight,
    Shortcut::SplitDown,
    Shortcut::NextTab,
    Shortcut::PreviousTab,
    Shortcut::CloseTerminal,
    Shortcut::Find,
    Shortcut::ToggleSidebar,
    Shortcut::Settings,
];

pub fn command(action: Shortcut) -> Option<&'static Command> {
    COMMANDS.iter().find(|command| command.action == action)
}

/// A command's name in `[rune.keys]`: `SplitRight` is `split-right`, and
/// the tabs are `tab-1` to `tab-9`.
fn id(action: Shortcut) -> String {
    if let Shortcut::Tab(index) = action {
        return format!("tab-{}", index + 1);
    }
    let mut id = String::new();
    for c in format!("{action:?}").chars() {
        if c.is_ascii_uppercase() && !id.is_empty() {
            id.push('-');
        }
        id.push(c.to_ascii_lowercase());
    }
    id
}

/// Chords from `[rune.keys]`; `None` takes the chord away from the table.
// ponytail: menus and the shortcut list still show the built-in chords;
// build them from this map too if people rebind enough to be misled.
static USER: std::sync::OnceLock<Vec<(String, Option<Shortcut>)>> = std::sync::OnceLock::new();

/// A chord as the table spells it: lower case, modifiers in `cmd`, `ctrl`,
/// `alt`, `shift` order, then the key.
fn canonical(chord: &str) -> String {
    let lower = chord.to_ascii_lowercase();
    let mut parts: Vec<&str> = lower.split('+').map(str::trim).collect();
    let key = parts.pop().unwrap_or_default();
    let held = ["cmd", "ctrl", "alt", "shift"]
        .into_iter()
        .filter(|name| parts.contains(name));
    held.chain([key]).collect::<Vec<_>>().join("+")
}

/// Read `[rune.keys]`: the bindings, and a line for each one not understood.
pub fn user_keys(
    keys: &std::collections::BTreeMap<String, String>,
) -> (Vec<(String, Option<Shortcut>)>, Vec<String>) {
    let mut problems = Vec::new();
    let mut bound = Vec::new();
    for (chord, name) in keys {
        let action = COMMANDS.iter().map(|command| command.action);
        match (name.as_str(), action.into_iter().find(|a| id(*a) == *name)) {
            ("none", _) => bound.push((canonical(chord), None)),
            (_, Some(action)) => bound.push((canonical(chord), Some(action))),
            (_, None) => problems.push(format!("{chord}: no command named {name}")),
        }
    }
    (bound, problems)
}

/// Install `[rune.keys]` for this run and return what was not understood.
pub fn bind(keys: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    let (bound, problems) = user_keys(keys);
    let _ = USER.set(bound);
    problems
}

fn lookup(user: &[(String, Option<Shortcut>)], chord: Chord) -> Option<Shortcut> {
    let spelled = chord.accelerator();
    if let Some((_, action)) = user.iter().find(|(key, _)| *key == spelled) {
        return *action;
    }
    COMMANDS
        .iter()
        .find(|command| command.chords().contains(&chord))
        .map(|command| command.action)
}

/// What a chord does here, if anything.
pub fn action_of(chord: Chord) -> Option<Shortcut> {
    lookup(USER.get().map_or(&[], Vec::as_slice), chord)
}

#[cfg(test)]
mod tests;
