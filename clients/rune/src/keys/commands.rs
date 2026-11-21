//! The one table of what the app can do: each command's title and the chords
//! that reach it on macOS and elsewhere. Key handling and the palette read
//! it, so a chord and what it says beside a command can't disagree.

use super::Shortcut;

mod case;
mod chord;
#[cfg(test)]
use case::PROPER;
pub use case::sentence_case;
pub use chord::{Cap, Chord};
mod keycaps;

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
    /// The command as the in-app lists (the palette, the phone's actions)
    /// say it: [`Self::title`] in sentence case.
    pub fn label(&self) -> String {
        sentence_case(self.title)
    }

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
    row!(Shortcut::Changes, "Show Changes", [], []),
    row!(Shortcut::Popup, "Popup…", [], []),
    row!(Shortcut::CopyConfig, "Copy Settings as TOML", [], []),
    row!(Shortcut::RestoreSessions, "Restore Sessions", [], []),
    row!(Shortcut::Galaxy, "Open Galaxy", [], []),
    row!(Shortcut::Reef, "Open Reef", [], []),
    row!(Shortcut::Pitch, "Open Pitch", [], []),
    row!(Shortcut::Kitchen, "Open Kitchen", [], []),
    row!(Shortcut::PitLane, "Open Pit Lane", [], []),
    row!(Shortcut::Hive, "Open Hive", [], []),
    row!(Shortcut::Playground, "Playground…", [], []),
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
    row!(
        Shortcut::Screenshot,
        "Screenshot to Terminal",
        [m('o').shift()],
        [l('o')]
    ),
    row!(mac Shortcut::QuickLook, "Quick Look", [m('y')]),
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
    row!(Shortcut::WhatWorksHere, "What Works Here", [], []),
    row!(Shortcut::PairPhone, "Pair a Phone", [], []),
    row!(Shortcut::RetryConnection, "Retry Connection", [], []),
    row!(Shortcut::CopyDiagnostics, "Copy Diagnostics", [], []),
    row!(Shortcut::RenameSession, "Rename Session", [], []),
    row!(Shortcut::RenameTab, "Rename Tab", [], []),
    row!(Shortcut::LockTab, "Lock or Unlock Tab", [], []),
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
    // Command+arrows walk the window like the desktop app did: across for
    // the session's terminals, up and down for sessions. Super+arrows belong
    // to the window manager elsewhere, so only the palette reaches them.
    row!(
        Shortcut::NextTerminalInSession,
        "Next Terminal in Session",
        [Chord::cmd(Cap::Right)],
        []
    ),
    row!(
        Shortcut::PreviousTerminalInSession,
        "Previous Terminal in Session",
        [Chord::cmd(Cap::Left)],
        []
    ),
    row!(
        Shortcut::NextSession,
        "Next Session",
        [Chord::cmd(Cap::Down)],
        []
    ),
    row!(
        Shortcut::PreviousSession,
        "Previous Session",
        [Chord::cmd(Cap::Up)],
        []
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
    // Shift beside Command+Left/Right, which walk the session's terminals.
    row!(
        Shortcut::NextSlotTab,
        "Next Tab in Split",
        [Chord::cmd(Cap::Right).shift()],
        [Chord::ctrl(Cap::Right).shift()]
    ),
    row!(
        Shortcut::PreviousSlotTab,
        "Previous Tab in Split",
        [Chord::cmd(Cap::Left).shift()],
        [Chord::ctrl(Cap::Left).shift()]
    ),
    row!(
        Shortcut::ToggleSidebar,
        "Toggle Sidebar",
        [m('b')],
        [l('b')]
    ),
    row!(
        Shortcut::FocusSidebar,
        "Focus Sidebar",
        [m('b').shift()],
        [l('b').alt()]
    ),
    row!(
        Shortcut::DisplayPanes,
        "Show Pane Numbers",
        [m('\'')],
        [l('\'')]
    ),
    // iTerm2's chord for typing into every pane of the tab.
    row!(
        Shortcut::SyncPanes,
        "Type in Every Pane of Tab",
        [m('i').alt()],
        [l('i').alt()]
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
        Shortcut::InterfaceBigger,
        "Bigger Interface",
        [m('=').alt(), m('=').shift().alt()],
        [l('=').alt()]
    ),
    row!(
        Shortcut::InterfaceSmaller,
        "Smaller Interface",
        [m('-').alt()],
        [l('-').alt()]
    ),
    row!(
        Shortcut::InterfaceReset,
        "Default Interface Size",
        [m('0').alt()],
        [l('0').alt()]
    ),
    row!(
        Shortcut::ScrollToTop,
        "Scroll to Top of History",
        [Chord::cmd(Cap::Home)],
        [Chord::ctrl(Cap::Home).shift()]
    ),
    row!(
        Shortcut::ScrollToBottom,
        "Scroll to Bottom",
        [m('l'), Chord::cmd(Cap::End)],
        [l('l'), Chord::ctrl(Cap::End).shift()]
    ),
    row!(
        Shortcut::PromptUp,
        "Jump to Previous Prompt",
        [Chord::cmd(Cap::Up).shift()],
        [Chord::ctrl(Cap::Up).shift()]
    ),
    row!(
        Shortcut::PromptDown,
        "Jump to Next Prompt",
        [Chord::cmd(Cap::Down).shift()],
        [Chord::ctrl(Cap::Down).shift()]
    ),
    // Bound in `[rune.keys]`; no chord of their own.
    row!(hidden Shortcut::ScrollPageUp, "Scroll Page Up", [], []),
    row!(hidden Shortcut::ScrollPageDown, "Scroll Page Down", [], []),
    row!(hidden Shortcut::ScrollHalfPageUp, "Scroll Half a Page Up", [], []),
    row!(hidden Shortcut::ScrollHalfPageDown, "Scroll Half a Page Down", [], []),
    row!(hidden Shortcut::ClearSelection, "Clear Selection", [], []),
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
/// The playground's scenes, listed under View → Playground rather than in View.
pub const PLAYGROUND: &[Shortcut] = &[
    Shortcut::Galaxy,
    Shortcut::Reef,
    Shortcut::Pitch,
    Shortcut::Kitchen,
    Shortcut::PitLane,
    Shortcut::Hive,
];

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

mod user;
pub use user::{action_of, bind};
#[cfg(test)]
use user::{lookup, user_keys};

#[cfg(test)]
mod tests;
