//! Rune keys as the terminal's structured key atoms.
//!
//! Rune resolves layout and modifiers; phux's libghostty encoder turns the
//! atom into bytes. Nothing here writes an escape sequence by hand.

use phux_client_runtime::control::keys::physical_key_for_char;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use rune::{Key, Mods, PhysicalKeyCode};

mod commands;
#[cfg(all(test, not(target_os = "macos")))]
mod linux_tests;
pub use commands::{COMMANDS, Command, ESSENTIALS, PLAYGROUND, bind, command};
use commands::{Cap, Chord};

/// What a key press should do in the terminal.
#[derive(Debug, Clone, PartialEq)]
pub enum Typed {
    /// Committed layout text with no Ctrl or Meta: type it.
    Text(String),
    /// A key the encoder resolves with its modifiers (Enter, arrows, Ctrl-C).
    Key(KeyEvent),
}

/// An app shortcut rather than terminal input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    Copy,
    Paste,
    NewTerminal,
    NewSession,
    NextTerminal,
    PreviousTerminal,
    /// The shown session's terminals only, tab by tab in pane order.
    NextTerminalInSession,
    PreviousTerminalInSession,
    /// The sessions in the sidebar's order.
    NextSession,
    PreviousSession,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    /// The sidebar, tabs, palette and Settings, not the terminal's text.
    InterfaceBigger,
    InterfaceSmaller,
    InterfaceReset,
    ScrollToTop,
    ScrollToBottom,
    /// The focused pane's history by a page (its height less a row), or half.
    ScrollPageUp,
    ScrollPageDown,
    ScrollHalfPageUp,
    ScrollHalfPageDown,
    /// In copy mode, drop the selection and stay where the cursor is.
    ClearSelection,
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
    NextSlotTab,
    PreviousSlotTab,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    Find,
    FindNext,
    FindPrevious,
    FindSelection,
    /// A keyboard cursor over the pane, to select and copy with vi keys.
    CopyMode,
    CopyConfig,
    /// Start again what a server restart ended.
    RestoreSessions,
    Inbox,
    /// What Git says changed in the focused terminal's checkout.
    Changes,
    /// One command in a floating terminal, closed when it exits.
    Popup,
    /// The fleet as a galaxy: machines, their agents, and how they relate.
    Galaxy,
    /// The fleet as a reef: machines as reefs, agents as fish.
    Reef,
    /// The fleet as football training: machines as squads, agents as players.
    Pitch,
    /// The fleet as a kitchen: machines as lines, agents as chefs.
    Kitchen,
    /// The fleet as a pit lane: machines as garages, agents as race cars.
    PitLane,
    /// The fleet as beehives: machines as comb frames, agents as bees.
    Hive,
    /// The palette over the six scenes above, as View's Playground.
    Playground,
    Settings,
    /// The themes, previewed live as the highlight moves.
    ChangeTheme,
    /// The sessions, to switch to one or create one.
    SwitchSession,
    /// A path on the pane's host, inserted at its prompt.
    InsertPath,
    /// A region of the screen, picked with the system's own tool, sent to
    /// the focused terminal the way a pasted image is.
    Screenshot,
    /// Preview the file a path in the output names.
    QuickLook,
    /// The tab at this index.
    Tab(u8),
    RenameSession,
    RenameTab,
    /// Lock the active tab against close, or unlock it.
    LockTab,
    CloseSession,
    CheckForUpdates,
    InstallUpdate,
    Machines,
    /// This machine's checklist: what works here, why not, and the fix.
    WhatWorksHere,
    /// Show a code a phone scans to pair with this computer.
    PairPhone,
    RetryConnection,
    CopyDiagnostics,
    ToggleFullScreen,
    KeyboardShortcuts,
    /// The handful of keys worth learning first, with the full list one row away.
    EssentialShortcuts,
    /// First-run onboarding, from the top.
    GetStarted,
    SelectAll,
    PromptUp,
    PromptDown,
    /// One pane, the terminal UI in it, and none of the window's chrome.
    Bare,
    /// A number over each pane of the tab; the next digit focuses that pane.
    DisplayPanes,
    /// The keyboard into the sidebar's rows; Esc hands it back.
    FocusSidebar,
    /// Typing goes to every terminal of the tab, until toggled off.
    SyncPanes,
}

impl Shortcut {
    /// Over the inbox or settings the terminal behind is out of reach: no
    /// pasting into it or closing it unseen.
    pub const fn works_over_sheets(self) -> bool {
        matches!(
            self,
            Self::Inbox
                | Self::Galaxy
                | Self::Reef
                | Self::Pitch
                | Self::Kitchen
                | Self::PitLane
                | Self::Hive
                | Self::Settings
                | Self::PairPhone
                | Self::ChangeTheme
                | Self::ZoomIn
                | Self::ZoomOut
                | Self::ZoomReset
                | Self::InterfaceBigger
                | Self::InterfaceSmaller
                | Self::InterfaceReset
                | Self::CheckForUpdates
                | Self::ToggleFullScreen
        )
    }
}

/// `key` pressed with no modifier held.
pub fn plain(key: Key) -> rune::KeyEvent {
    rune::KeyEvent {
        key,
        mods: Mods::default(),
    }
}

pub fn mods(mods: Mods) -> ModSet {
    let mut set = ModSet::empty();
    set.set(ModSet::SHIFT, mods.shift);
    set.set(ModSet::CTRL, mods.ctrl);
    set.set(ModSet::ALT, mods.alt);
    set.set(ModSet::SUPER, mods.logo);
    set
}

fn atom(key: PhysicalKey, mods: ModSet, base: Option<char>) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: base.map(u32::from),
    }
}

fn named(key: &Key) -> Option<PhysicalKey> {
    use PhysicalKey as K;
    Some(match key {
        Key::Enter => K::Enter,
        Key::Backspace => K::Backspace,
        Key::Delete => K::Delete,
        Key::Left => K::ArrowLeft,
        Key::Right => K::ArrowRight,
        Key::Up => K::ArrowUp,
        Key::Down => K::ArrowDown,
        Key::Home => K::Home,
        Key::End => K::End,
        Key::PageUp => K::PageUp,
        Key::PageDown => K::PageDown,
        Key::Insert => K::Insert,
        Key::PrintScreen => K::PrintScreen,
        Key::ScrollLock => K::ScrollLock,
        Key::Pause => K::Pause,
        Key::Escape => K::Escape,
        Key::Tab => K::Tab,
        Key::Function(n) => function(*n)?,
        _ => return None,
    })
}

fn function(n: u8) -> Option<PhysicalKey> {
    use PhysicalKey as K;
    const KEYS: [PhysicalKey; 12] = [
        K::F1,
        K::F2,
        K::F3,
        K::F4,
        K::F5,
        K::F6,
        K::F7,
        K::F8,
        K::F9,
        K::F10,
        K::F11,
        K::F12,
    ];
    KEYS.get(usize::from(n).checked_sub(1)?).copied()
}

/// The US-layout character of a physical key: what a chord means whatever
/// the active layout types there (Ctrl-C on a Cyrillic layout is still ^C).
pub fn us_char(code: PhysicalKeyCode) -> Option<char> {
    use PhysicalKeyCode as C;
    const LETTERS: [PhysicalKeyCode; 26] = [
        C::KeyA,
        C::KeyB,
        C::KeyC,
        C::KeyD,
        C::KeyE,
        C::KeyF,
        C::KeyG,
        C::KeyH,
        C::KeyI,
        C::KeyJ,
        C::KeyK,
        C::KeyL,
        C::KeyM,
        C::KeyN,
        C::KeyO,
        C::KeyP,
        C::KeyQ,
        C::KeyR,
        C::KeyS,
        C::KeyT,
        C::KeyU,
        C::KeyV,
        C::KeyW,
        C::KeyX,
        C::KeyY,
        C::KeyZ,
    ];
    const DIGITS: [PhysicalKeyCode; 10] = [
        C::Digit0,
        C::Digit1,
        C::Digit2,
        C::Digit3,
        C::Digit4,
        C::Digit5,
        C::Digit6,
        C::Digit7,
        C::Digit8,
        C::Digit9,
    ];
    if let Some(i) = LETTERS.iter().position(|k| *k == code) {
        return char::from_u32('a' as u32 + i as u32);
    }
    if let Some(i) = DIGITS.iter().position(|k| *k == code) {
        return char::from_digit(i as u32, 10);
    }
    Some(match code {
        C::Minus => '-',
        C::Equal => '=',
        C::BracketLeft => '[',
        C::BracketRight => ']',
        C::Backslash => '\\',
        C::Semicolon => ';',
        C::Quote => '\'',
        C::Comma => ',',
        C::Period => '.',
        C::Slash => '/',
        C::Backquote => '`',
        C::Space => ' ',
        _ => return None,
    })
}

/// The app's action for a press, if it is exactly a chord in the table.
/// `physical` names the key itself, so a shifted chord (Ctrl-Shift-minus
/// arrives as `_`) and other layouts still match.
pub fn shortcut(key: &Key, mods: Mods, physical: Option<PhysicalKeyCode>) -> Option<Shortcut> {
    let cap = match key {
        // An Option chord types a character on macOS (⌥; is …): the key
        // under it is the chord.
        Key::Text(_) if mods.alt && !mods.cmd && !mods.ctrl => {
            Cap::Char(physical.and_then(us_char)?)
        }
        _ => cap(key, physical)?,
    };
    commands::action_of(Chord {
        cap,
        // Rune's `cmd` is Ctrl off macOS, which the table names as Ctrl.
        command: if cfg!(any(target_os = "macos", target_os = "ios")) {
            mods.cmd
        } else {
            mods.logo
        },
        ctrl: mods.ctrl,
        alt: mods.alt,
        shift: mods.shift,
    })
}

/// The chord key of a press. Letters and digits arrive only as `Command`:
/// plain typing never reaches the table.
fn cap(key: &Key, physical: Option<PhysicalKeyCode>) -> Option<Cap> {
    Some(match key {
        Key::Command(c) => Cap::Char(unshifted(physical.and_then(us_char).unwrap_or(*c))),
        Key::Enter => Cap::Enter,
        Key::Left => Cap::Left,
        Key::Right => Cap::Right,
        Key::Up => Cap::Up,
        Key::Down => Cap::Down,
        Key::Home => Cap::Home,
        Key::End => Cap::End,
        Key::PageUp => Cap::PageUp,
        Key::PageDown => Cap::PageDown,
        Key::Insert => Cap::Insert,
        _ => return None,
    })
}

/// The US key a shifted symbol sits on, for a press with no physical key.
const fn unshifted(c: char) -> char {
    match c {
        '<' => ',',
        '{' => '[',
        '}' => ']',
        '+' => '=',
        '_' => '-',
        ')' => '0',
        other => other,
    }
}

/// The physical key and unshifted character a Ctrl or Meta chord encodes:
/// the key's own position when known, else the layout's character.
fn chord(physical: Option<PhysicalKeyCode>, fallback: Option<char>) -> Option<(PhysicalKey, char)> {
    let ch = physical
        .and_then(us_char)
        .or(fallback)?
        .to_ascii_lowercase();
    let key = if ch == ' ' {
        PhysicalKey::Space
    } else {
        physical_key_for_char(ch)
    };
    Some((key, ch))
}

/// Translate one key down. `base` is the character the key types with no
/// modifiers (Rune's `Tx::key_base`) and `physical` the key itself
/// (`Tx::physical_key`). `None`: not terminal input.
pub fn typed(
    key: &Key,
    key_mods: Mods,
    base: Option<char>,
    physical: Option<PhysicalKeyCode>,
    option_as_meta: bool,
) -> Option<Typed> {
    let set = mods(key_mods);
    if let Some(physical) = named(key) {
        return Some(Typed::Key(atom(physical, set, None)));
    }
    match key {
        Key::Text(text) if !key_mods.ctrl && !alt_is_meta(key_mods, option_as_meta) => {
            Some(Typed::Text(text.clone()))
        }
        Key::Text(text) => {
            let (key, ch) = chord(physical, base.or_else(|| text.chars().next()))?;
            Some(Typed::Key(atom(key, set, Some(ch))))
        }
        // Off macOS a Ctrl chord arrives as `Command`; it is terminal input
        // unless `shortcut` claimed it.
        Key::Command(c) if !cfg!(any(target_os = "macos", target_os = "ios")) => {
            let (key, ch) = chord(physical, Some(base.unwrap_or(*c)))?;
            Some(Typed::Key(atom(key, set, Some(ch))))
        }
        _ => None,
    }
}

/// macOS Option types layout characters (å, ∑), as Ghostty's default does,
/// unless the person turned on Option as Meta; elsewhere Alt is Meta.
const fn alt_is_meta(mods: Mods, option_as_meta: bool) -> bool {
    mods.alt && (option_as_meta || !cfg!(any(target_os = "macos", target_os = "ios")))
}

#[cfg(test)]
mod tests;
