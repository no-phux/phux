//! Chord normalization and the built-in command chords.
//!
//! Ghostty binds overlay these only while display preferences keep
//! `ghostty_keys`. A `global:` bind of `quick-terminal` is the system-wide
//! quick terminal chord.

use super::ghostty::{Bind, GhosttyConfig, Origin};
use std::sync::LazyLock;

mod catalog;
use catalog::CATALOG;

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
mod tests;
