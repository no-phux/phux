//! The one table of what the app can do: each command's title and the chords
//! that reach it on macOS and elsewhere. Key handling and the palette read
//! it, so a chord and what it says beside a command can't disagree.

use super::Shortcut;

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

    const fn alt(self) -> Self {
        Self { alt: true, ..self }
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
        if cfg!(target_os = "macos") {
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
        if cfg!(target_os = "macos") {
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
            listed: cfg!(target_os = "macos"),
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
    row!(Shortcut::Settings, "Settings", [m(',')], [l(',')]),
    row!(Shortcut::KeyboardShortcuts, "Keyboard Shortcuts", [], []),
    row!(Shortcut::CheckForUpdates, "Check for Updates", [], []),
    row!(Shortcut::InstallUpdate, "Update Now", [], []),
    row!(Shortcut::Machines, "Switch or Add Machine", [], []),
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
    row!(Shortcut::Copy, "Copy Selection", [m('c')], [l('c')]),
    row!(Shortcut::Paste, "Paste", [m('v')], [l('v')]),
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
pub fn command(action: Shortcut) -> Option<&'static Command> {
    COMMANDS.iter().find(|command| command.action == action)
}

/// What a chord does here, if anything.
pub fn action_of(chord: Chord) -> Option<Shortcut> {
    COMMANDS
        .iter()
        .find(|command| command.chords().contains(&chord))
        .map(|command| command.action)
}

#[cfg(test)]
mod tests {
    use rune::{Key, Mods};

    use super::*;
    use crate::keys::shortcut;

    /// The press a chord describes, as Rune reports it here.
    fn press(chord: Chord) -> (Key, Mods) {
        let mods = Mods {
            cmd: chord.command || (chord.ctrl && !cfg!(target_os = "macos")),
            ctrl: chord.ctrl,
            alt: chord.alt,
            shift: chord.shift,
            logo: chord.command,
        };
        let key = match chord.cap {
            Cap::Char(c) => Key::Command(c),
            Cap::Enter => Key::Enter,
            Cap::Left => Key::Left,
            Cap::Right => Key::Right,
            Cap::Up => Key::Up,
            Cap::Down => Key::Down,
            Cap::Home => Key::Home,
            Cap::End => Key::End,
            Cap::PageUp => Key::PageUp,
            Cap::PageDown => Key::PageDown,
        };
        (key, mods)
    }

    #[test]
    fn every_listed_chord_reaches_its_action() {
        for command in COMMANDS {
            for chord in command.chords() {
                let (key, mods) = press(*chord);
                assert_eq!(
                    shortcut(&key, mods, None),
                    Some(command.action),
                    "{chord:?}"
                );
            }
        }
    }

    #[test]
    fn no_chord_means_two_things() {
        let all: Vec<_> = COMMANDS.iter().flat_map(Command::chords).collect();
        for (at, chord) in all.iter().enumerate() {
            assert!(!all[at + 1..].contains(chord), "{chord:?}");
        }
    }

    #[test]
    fn every_mac_chord_is_an_accelerator() {
        for chord in COMMANDS.iter().flat_map(|command| command.mac) {
            let parsed = chord.accelerator().parse::<rune::Shortcut>();
            assert!(parsed.is_ok(), "{chord:?}");
        }
    }

    #[test]
    fn labels_read_as_each_platform_writes_them() {
        assert_eq!(m('w').shift().mac_label(), "⇧⌘W");
        assert_eq!(l('w').alt().linux_label(), "Ctrl+Alt+Shift+W");
        assert_eq!(
            Chord::ctrl(Cap::PageDown).shift().linux_label(),
            "Ctrl+Shift+PgDn"
        );
    }

    #[test]
    fn linux_chords_never_touch_plain_ctrl_letters() {
        // Ctrl-C and its kin belong to the shell.
        for chord in COMMANDS.iter().flat_map(|command| command.linux) {
            assert!(
                chord.shift || chord.alt || !matches!(chord.cap, Cap::Char(_)),
                "{chord:?}"
            );
        }
    }
}
