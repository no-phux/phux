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
        Cap::Insert => Key::Insert,
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

/// Names every variant twice: as a list to walk, and as a match that stops
/// compiling when a new variant is missing from it.
macro_rules! every_shortcut {
    ($($variant:ident),* $(,)?) => {
        const EVERY_SHORTCUT: &[Shortcut] = &[$(Shortcut::$variant),*];

        const fn _is_listed(action: Shortcut) {
            match action {
                $(Shortcut::$variant)|* | Shortcut::Tab(_) => {}
            }
        }
    };
}

every_shortcut!(
    Copy,
    Paste,
    NewTerminal,
    NewSession,
    NextTerminal,
    PreviousTerminal,
    NextTerminalInSession,
    PreviousTerminalInSession,
    NextSession,
    PreviousSession,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ScrollTop,
    ScrollBottom,
    Palette,
    ToggleSidebar,
    JumpToAttention,
    SplitRight,
    SplitDown,
    ZoomPane,
    CloseTerminal,
    CloseTab,
    NextTab,
    PreviousTab,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    Find,
    FindNext,
    FindPrevious,
    FindSelection,
    CopyMode,
    CopyConfig,
    RestoreSessions,
    Inbox,
    Changes,
    Popup,
    Galaxy,
    Reef,
    Pitch,
    Kitchen,
    PitLane,
    Hive,
    Settings,
    ChangeTheme,
    SwitchSession,
    InsertPath,
    RenameSession,
    RenameTab,
    LockTab,
    CloseSession,
    CheckForUpdates,
    InstallUpdate,
    Machines,
    RetryConnection,
    CopyDiagnostics,
    ToggleFullScreen,
    KeyboardShortcuts,
    EssentialShortcuts,
    GetStarted,
    SelectAll,
    PromptUp,
    PromptDown,
    Bare,
);

/// An essential with no chord here would teach a key that does nothing.
#[test]
fn every_essential_has_a_chord_on_both_platforms() {
    for action in ESSENTIALS {
        let command = command(*action).unwrap_or_else(|| panic!("{action:?} has no row"));
        assert!(!command.mac.is_empty(), "{action:?} has no macOS chord");
        assert!(!command.linux.is_empty(), "{action:?} has no Linux chord");
    }
    for (i, action) in ESSENTIALS.iter().enumerate() {
        assert!(
            !ESSENTIALS[..i].contains(action),
            "{action:?} is listed twice"
        );
    }
}

#[test]
fn every_shortcut_has_a_row() {
    let tabs = (0..9).map(Shortcut::Tab);
    for action in EVERY_SHORTCUT.iter().copied().chain(tabs) {
        assert!(command(action).is_some(), "{action:?}");
    }
}

#[test]
fn option_semicolon_inserts_a_path_though_it_types_a_character() {
    let mods = Mods {
        alt: true,
        ..Mods::default()
    };
    let typed = Key::Text("…".to_owned());
    let at = Some(rune::PhysicalKeyCode::Semicolon);
    if cfg!(target_os = "macos") {
        assert_eq!(shortcut(&typed, mods, at), Some(Shortcut::InsertPath));
    }
    let other = Some(rune::PhysicalKeyCode::KeyA);
    assert_eq!(shortcut(&Key::Text("å".to_owned()), mods, other), None);
}

#[test]
fn full_screen_is_a_macos_chord() {
    let row = command(Shortcut::ToggleFullScreen).expect("a row");
    assert_eq!(row.mac[0].mac_label(), "⌃⌘F");
    assert_eq!(row.mac[0].accelerator(), "cmd+ctrl+f");
    assert!(row.linux.is_empty());
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

#[test]
fn config_keys_add_a_chord_take_one_away_and_report_a_bad_name() {
    let keys = [
        ("Shift+Cmd+J", "split-right"),
        ("cmd+d", "none"),
        ("cmd+y", "no-such-thing"),
        ("ctrl+9", "tab-3"),
    ]
    .into_iter()
    .map(|(chord, name)| (chord.to_owned(), name.to_owned()))
    .collect();
    let (user, problems) = user_keys(&keys);
    assert_eq!(problems, ["cmd+y: no command named no-such-thing"]);
    assert_eq!(
        lookup(&user, Chord::cmd(Cap::Char('j')).shift()),
        Some(Shortcut::SplitRight)
    );
    assert_eq!(lookup(&user, Chord::cmd(Cap::Char('d'))), None);
    assert_eq!(
        lookup(&user, Chord::ctrl(Cap::Char('9'))),
        Some(Shortcut::Tab(2))
    );
    // Every command has a name, and no two share one.
    let mut ids: Vec<String> = COMMANDS.iter().map(|command| id(command.action)).collect();
    ids.sort();
    let count = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), count);
}
