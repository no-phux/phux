//! The one table of what the app can do: each command's title and the chords
//! that reach it on macOS and elsewhere. The palette reads it; a test holds
//! the key handling to it.

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

    const fn ctrl_shift(c: char) -> Self {
        Self {
            cap: Cap::Char(c),
            command: false,
            ctrl: true,
            alt: false,
            shift: true,
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

    /// The macOS label: `⌘⇧D`.
    pub fn mac_label(self) -> String {
        let mut label = String::new();
        for (held, mark) in [
            (self.ctrl, '⌃'),
            (self.command, '⌘'),
            (self.alt, '⌥'),
            (self.shift, '⇧'),
        ] {
            if held {
                label.push(mark);
            }
        }
        label.push(match self.cap {
            Cap::Char(c) => c.to_ascii_uppercase(),
            Cap::Enter => '↩',
            Cap::Left => '←',
            Cap::Right => '→',
            Cap::Up => '↑',
            Cap::Down => '↓',
            Cap::Home => '↖',
            Cap::End => '↘',
        });
        label
    }
}

/// One thing the app can do.
#[derive(Debug)]
pub struct Command {
    pub action: Shortcut,
    pub title: &'static str,
    /// Its chords, the first shown beside it; aliases follow.
    pub mac: &'static [Chord],
    #[cfg_attr(not(test), allow(dead_code, reason = "keys dispatch through it next"))]
    pub linux: &'static [Chord],
    /// Whether the palette offers it.
    pub listed: bool,
}

const fn m(c: char) -> Chord {
    Chord::cmd(Cap::Char(c))
}

const fn l(c: char) -> Chord {
    Chord::ctrl_shift(c)
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
    ($index:expr, $digit:expr) => {
        row!(hidden Shortcut::Tab($index), "", [m($digit)], [l($digit)])
    };
}

pub const COMMANDS: &[Command] = &[
    row!(Shortcut::NewTerminal, "New tab", [m('t')], [l('t')]),
    row!(Shortcut::NewSession, "New session", [m('n')], [l('n')]),
    row!(
        Shortcut::JumpToAttention,
        "Jump to what needs you",
        [m('j')],
        [l('j')]
    ),
    row!(Shortcut::Inbox, "Open inbox", [m('i')], [l('i')]),
    row!(Shortcut::Settings, "Settings", [m(',')], [l(',')]),
    row!(Shortcut::CheckForUpdates, "Check for updates", [], []),
    row!(Shortcut::InstallUpdate, "Update now", [], []),
    row!(Shortcut::Machines, "Switch or add machine", [], []),
    row!(Shortcut::RenameSession, "Rename session", [], []),
    row!(Shortcut::RenameTab, "Rename tab", [], []),
    row!(Shortcut::CloseSession, "Close session", [], []),
    row!(Shortcut::SplitRight, "Split right", [m('d')], [l('d')]),
    row!(
        Shortcut::SplitDown,
        "Split down",
        [m('d').shift()],
        [l('e')]
    ),
    row!(
        Shortcut::ZoomPane,
        "Zoom pane",
        [Chord::cmd(Cap::Enter).shift()],
        []
    ),
    row!(
        Shortcut::CloseTerminal,
        "Close terminal",
        [m('w')],
        [l('w')]
    ),
    row!(Shortcut::Find, "Find in terminal", [m('f')], [l('f')]),
    row!(Shortcut::FindSelection, "Find selected text", [m('e')], []),
    row!(
        Shortcut::FocusLeft,
        "Focus pane left",
        [Chord::cmd(Cap::Left).alt()],
        []
    ),
    row!(
        Shortcut::FocusRight,
        "Focus pane right",
        [Chord::cmd(Cap::Right).alt()],
        []
    ),
    row!(
        Shortcut::FocusUp,
        "Focus pane up",
        [Chord::cmd(Cap::Up).alt()],
        []
    ),
    row!(
        Shortcut::FocusDown,
        "Focus pane down",
        [Chord::cmd(Cap::Down).alt()],
        []
    ),
    row!(Shortcut::NextTerminal, "Next terminal", [m(']')], [l(']')]),
    row!(
        Shortcut::PreviousTerminal,
        "Previous terminal",
        [m('[')],
        [l('[')]
    ),
    row!(
        Shortcut::ToggleSidebar,
        "Toggle sidebar",
        [m('b')],
        [l('b')]
    ),
    row!(Shortcut::Copy, "Copy selection", [m('c')], [l('c')]),
    row!(Shortcut::Paste, "Paste", [m('v')], [l('v')]),
    row!(Shortcut::ZoomIn, "Bigger text", [m('=')], [l('=')]),
    row!(Shortcut::ZoomOut, "Smaller text", [m('-')], [l('-')]),
    row!(Shortcut::ZoomReset, "Default text size", [m('0')], [l('0')]),
    row!(
        Shortcut::ScrollTop,
        "Scroll to top of history",
        [Chord::cmd(Cap::Up), Chord::cmd(Cap::Home)],
        []
    ),
    row!(
        Shortcut::ScrollBottom,
        "Scroll to bottom",
        [Chord::cmd(Cap::Down), Chord::cmd(Cap::End)],
        []
    ),
    row!(hidden Shortcut::Palette, "Command palette", [m('p'), m('k')], [l('p'), l('k')]),
    row!(hidden Shortcut::FindNext, "Find next", [m('g')], [l('g')]),
    row!(hidden Shortcut::FindPrevious, "Find previous", [m('g').shift()], []),
    tab!(0, '1'),
    tab!(1, '2'),
    tab!(2, '3'),
    tab!(3, '4'),
    tab!(4, '5'),
    tab!(5, '6'),
    tab!(6, '7'),
    tab!(7, '8'),
    tab!(8, '9'),
];

#[cfg(test)]
mod tests {
    use rune::{Key, Mods};

    use super::*;
    use crate::keys::{scroll_shortcut, shortcut};

    /// This platform's chords of a command.
    const fn chords(command: &Command) -> &'static [Chord] {
        if cfg!(target_os = "macos") {
            command.mac
        } else {
            command.linux
        }
    }

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
        };
        (key, mods)
    }

    #[test]
    fn every_listed_chord_reaches_its_action() {
        for command in COMMANDS {
            for chord in chords(command) {
                let (key, mods) = press(*chord);
                let got = shortcut(&key, mods, None).or_else(|| scroll_shortcut(&key, mods));
                assert_eq!(got, Some(command.action), "{chord:?}");
            }
        }
    }

    #[test]
    fn no_chord_means_two_things() {
        let all: Vec<_> = COMMANDS.iter().flat_map(chords).collect();
        for (at, chord) in all.iter().enumerate() {
            assert!(!all[at + 1..].contains(chord), "{chord:?}");
        }
    }
}
