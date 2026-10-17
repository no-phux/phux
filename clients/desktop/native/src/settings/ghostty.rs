//! Ghostty config: font, colour, padding, and keybinds mapped onto this app.
//!
//! Parsing is total. Unknown keys are ignored. A malformed line is skipped
//! with a [`Diagnostic`] in `diagnostics`; a value Ghostty accepts but this app
//! cannot represent is skipped with one in `unsupported`. Every other line still
//! applies. Later lines win. The theme file is read first so the config's own
//! lines override it. Each applied key and bind keeps the file and line that set it.

use super::display::DisplayPrefs;
use super::theme::{Theme, mix};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// What a Ghostty config specified. Absent fields do not override preferences.
#[derive(Debug, Clone, PartialEq)]
pub struct GhosttyConfig {
    /// Primary font family.
    pub font_family: Option<String>,
    /// Font size in points.
    pub font_size: Option<f64>,
    /// Cell width multiplier from a percentage adjustment.
    pub cell_width: Option<f64>,
    /// Cell height multiplier from a percentage adjustment.
    pub cell_height: Option<f64>,
    /// Background `#rrggbb`.
    pub background: Option<String>,
    /// Foreground `#rrggbb`.
    pub foreground: Option<String>,
    /// Cursor `#rrggbb`.
    pub cursor: Option<String>,
    /// Selection background.
    pub selection_background: Option<String>,
    /// Selection foreground.
    pub selection_foreground: Option<String>,
    /// Palette slots 0..=15. Missing slots stay `None`.
    pub palette: [Option<String>; 16],
    /// Horizontal padding.
    pub padding_x: Option<f64>,
    /// Vertical padding.
    pub padding_y: Option<f64>,
    /// Unfocused split opacity.
    pub unfocused_split_opacity: Option<f64>,
    /// Split divider colour.
    pub split_divider_color: Option<String>,
    /// Option sends Alt.
    pub option_as_alt: Option<bool>,
    /// Chord to command id. An empty command unbinds.
    pub keybinds: HashMap<String, String>,
    /// Chords bound with Ghostty's `global:` prefix, in insertion order.
    pub globals: Vec<String>,
    /// Lines this app cannot apply.
    pub unmapped: Vec<String>,
    /// The line that set each applied key, by Ghostty key name.
    pub origins: HashMap<String, Origin>,
    /// Every applied `keybind` line in config order, unbinds included. Later
    /// entries for the same chord win; see [`super::keymap::keymap_conflicts`].
    pub binds: Vec<Bind>,
    /// Lines skipped because they are malformed: no `=`, or a value Ghostty
    /// itself would not parse.
    pub diagnostics: Vec<Diagnostic>,
    /// Valid Ghostty lines for a key this app reads, with a value it cannot
    /// represent (a palette slot above 15, a named colour, a pixel cell
    /// adjustment, a value outside this app's range).
    pub unsupported: Vec<Diagnostic>,
}

/// The file and 1-based line a Ghostty value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// Source file; `None` for text parsed without one.
    pub file: Option<PathBuf>,
    /// 1-based line within `file`.
    pub line: usize,
}

impl Origin {
    /// `~/.config/ghostty/config:12`, or `line 12` without a file.
    #[must_use]
    pub fn label(&self) -> String {
        let Some(file) = &self.file else {
            return format!("line {}", self.line);
        };
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let shown = home
            .as_deref()
            .and_then(|home| file.strip_prefix(home).ok())
            .map(|rest| Path::new("~").join(rest))
            .unwrap_or_else(|| file.clone());
        format!("{}:{}", shown.display(), self.line)
    }
}

/// One applied `keybind` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bind {
    /// Normalized chord, as in [`GhosttyConfig::keybinds`].
    pub chord: String,
    /// Command id. Empty unbinds.
    pub command: String,
    /// The line that bound it.
    pub origin: Origin,
}

/// A line the parser skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Where the line is.
    pub origin: Origin,
    /// The trimmed line.
    pub text: String,
    /// Why it was skipped.
    pub reason: &'static str,
}

/// One file of Ghostty config text, in the order Ghostty reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhosttySource {
    /// The file the text was read from; `None` for inline text.
    pub path: Option<PathBuf>,
    /// File contents.
    pub text: String,
}

impl Default for GhosttyConfig {
    fn default() -> Self {
        Self {
            font_family: None,
            font_size: None,
            cell_width: None,
            cell_height: None,
            background: None,
            foreground: None,
            cursor: None,
            selection_background: None,
            selection_foreground: None,
            palette: [(); 16].map(|()| None),
            padding_x: None,
            padding_y: None,
            unfocused_split_opacity: None,
            split_divider_color: None,
            option_as_alt: None,
            keybinds: HashMap::new(),
            globals: Vec::new(),
            unmapped: Vec::new(),
            origins: HashMap::new(),
            binds: Vec::new(),
            diagnostics: Vec::new(),
            unsupported: Vec::new(),
        }
    }
}

/// Parse Ghostty config text with no file attached. Never fails.
#[must_use]
pub fn parse_ghostty(text: &str) -> GhosttyConfig {
    parse_ghostty_sources(&[GhosttySource {
        path: None,
        text: text.to_owned(),
    }])
}

/// Parse Ghostty config files in read order, keeping each value's origin.
/// Never fails: a skipped line becomes a [`Diagnostic`] and the rest apply.
#[must_use]
pub fn parse_ghostty_sources(sources: &[GhosttySource]) -> GhosttyConfig {
    let mut config = GhosttyConfig::default();
    let mut font_reset = false;
    for source in sources {
        // A lone CR also ends a line; CRLF counts once.
        for (index, physical) in source.text.split('\n').enumerate() {
            let origin = Origin {
                file: source.path.clone(),
                line: index + 1,
            };
            for raw in physical.split('\r') {
                parse_line(&mut config, &mut font_reset, raw.trim(), &origin);
            }
        }
    }
    config
}

fn parse_line(config: &mut GhosttyConfig, font_reset: &mut bool, line: &str, origin: &Origin) {
    if line.is_empty() || line.starts_with('#') {
        return;
    }
    let Some(equals) = line.find('=') else {
        config.diagnostics.push(Diagnostic {
            origin: origin.clone(),
            text: line.to_owned(),
            reason: "expected key = value",
        });
        return;
    };
    let key = line[..equals].trim();
    let value = unquote(line[equals + 1..].trim());
    if key == "font-family" {
        set_font_family(config, font_reset, value, origin);
        return;
    }
    match apply(config, key, &value, origin) {
        Applied::Yes => {
            config.origins.insert(key.to_owned(), origin.clone());
        }
        Applied::Malformed => config.diagnostics.push(Diagnostic {
            origin: origin.clone(),
            text: line.to_owned(),
            reason: "value is malformed",
        }),
        Applied::Unsupported(reason) => config.unsupported.push(Diagnostic {
            origin: origin.clone(),
            text: line.to_owned(),
            reason,
        }),
        Applied::Unknown => {}
    }
}

/// The first `font-family` wins until an empty one resets the list.
fn set_font_family(
    config: &mut GhosttyConfig,
    font_reset: &mut bool,
    value: String,
    origin: &Origin,
) {
    if value.is_empty() {
        *font_reset = true;
        config.font_family = None;
        config.origins.remove("font-family");
    } else if config.font_family.is_none() || *font_reset {
        config.font_family = Some(value);
        config
            .origins
            .insert("font-family".to_owned(), origin.clone());
        *font_reset = false;
    }
}

/// Whether a line's key took its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Applied {
    /// Stored.
    Yes,
    /// A key this app reads, with a value Ghostty would not parse either.
    Malformed,
    /// A value Ghostty accepts that this app cannot represent, and why.
    Unsupported(&'static str),
    /// A key this app does not read, or a keybind (whose origin `binds` keeps).
    Unknown,
}

const OUT_OF_RANGE: &str = "valid in Ghostty, outside this app's range";
const NAMED_COLOR: &str = "valid in Ghostty; this app reads hex colours only";
const PALETTE_SLOT: &str = "valid in Ghostty; this app keeps palette slots 0-15";
const PIXEL_CELL: &str = "valid in Ghostty; this app reads percent cell adjustments only";

fn apply(config: &mut GhosttyConfig, key: &str, value: &str, origin: &Origin) -> Applied {
    match key {
        "font-size" => set_number(value, 4.0, 72.0, |size| config.font_size = Some(size)),
        "adjust-cell-width" => set_percent(value, |scale| config.cell_width = Some(scale)),
        "adjust-cell-height" => set_percent(value, |scale| config.cell_height = Some(scale)),
        "background"
        | "foreground"
        | "cursor-color"
        | "selection-background"
        | "selection-foreground"
        | "split-divider-color" => set_color(config, key, value),
        "palette" => set_palette(config, value),
        "window-padding-x" => set_number(first_number(value), 0.0, 64.0, |pad| {
            config.padding_x = Some(pad);
        }),
        "window-padding-y" => set_number(first_number(value), 0.0, 64.0, |pad| {
            config.padding_y = Some(pad);
        }),
        // Ghostty clamps this one rather than rejecting it.
        "unfocused-split-opacity" => set_number(value, f64::MIN, f64::MAX, |opacity| {
            config.unfocused_split_opacity = Some(opacity.clamp(0.15, 1.0));
        }),
        "macos-option-as-alt" => {
            config.option_as_alt = Some(matches!(value, "true" | "left" | "right"));
            Applied::Yes
        }
        "keybind" if set_keybind(config, value, origin) => Applied::Unknown,
        "keybind" => Applied::Malformed,
        _ => Applied::Unknown,
    }
}

fn set_color(config: &mut GhosttyConfig, key: &str, value: &str) -> Applied {
    let color = match parse_color(value) {
        Ok(color) => color,
        Err(skipped) => return skipped,
    };
    let slot = match key {
        "background" => &mut config.background,
        "foreground" => &mut config.foreground,
        "cursor-color" => &mut config.cursor,
        "selection-background" => &mut config.selection_background,
        "selection-foreground" => &mut config.selection_foreground,
        "split-divider-color" => &mut config.split_divider_color,
        _ => return Applied::Unknown,
    };
    *slot = Some(color);
    Applied::Yes
}

/// Ghostty has 256 palette slots; this app keeps the first 16.
fn set_palette(config: &mut GhosttyConfig, value: &str) -> Applied {
    let Some((index, color)) = value.split_once('=') else {
        return Applied::Malformed;
    };
    let Ok(index) = index.trim().parse::<u8>() else {
        return Applied::Malformed;
    };
    let color = match parse_color(color) {
        Ok(color) => color,
        Err(skipped) => return skipped,
    };
    let Some(slot) = config.palette.get_mut(usize::from(index)) else {
        return Applied::Unsupported(PALETTE_SLOT);
    };
    *slot = Some(color);
    Applied::Yes
}

/// A hex colour, or why the value is skipped. Ghostty also accepts X11
/// colour names, which this app does not resolve.
fn parse_color(value: &str) -> Result<String, Applied> {
    if let Some(color) = hex_color(value) {
        return Ok(color);
    }
    if is_color_name(value) {
        return Err(Applied::Unsupported(NAMED_COLOR));
    }
    Err(Applied::Malformed)
}

/// Shaped like an X11 colour name: a letter, then letters, digits and spaces.
fn is_color_name(value: &str) -> bool {
    let value = value.trim();
    value.starts_with(|first: char| first.is_ascii_alphabetic())
        && value
            .chars()
            .all(|part| part.is_ascii_alphanumeric() || part == ' ')
}

fn hex_color(value: &str) -> Option<String> {
    let trimmed = value.trim().to_lowercase();
    let body = trimmed.strip_prefix('#').unwrap_or(&trimmed);
    if body.len() == 6 && body.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Some(format!("#{body}"));
    }
    if body.len() == 3 && body.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        let expanded: String = body.chars().flat_map(|digit| [digit, digit]).collect();
        return Some(format!("#{expanded}"));
    }
    None
}

fn set_number(value: &str, low: f64, high: f64, set: impl FnOnce(f64)) -> Applied {
    let Ok(number) = value.parse::<f64>() else {
        return Applied::Malformed;
    };
    if !number.is_finite() {
        return Applied::Malformed;
    }
    if !(low..=high).contains(&number) {
        return Applied::Unsupported(OUT_OF_RANGE);
    }
    set(number);
    Applied::Yes
}

/// Ghostty takes a percent (`10%`) or a whole number of pixels (`2`); this
/// app scales cells, so only the percent form applies.
fn set_percent(value: &str, set: impl FnOnce(f64)) -> Applied {
    let value = value.trim();
    let Some(body) = value.strip_suffix('%') else {
        return if value.parse::<i32>().is_ok() {
            Applied::Unsupported(PIXEL_CELL)
        } else {
            Applied::Malformed
        };
    };
    let Ok(percent) = body.parse::<f64>() else {
        return Applied::Malformed;
    };
    if !percent.is_finite() {
        return Applied::Malformed;
    }
    let scale = 1.0 + percent / 100.0;
    if !(0.5..=2.0).contains(&scale) {
        return Applied::Unsupported(OUT_OF_RANGE);
    }
    set(scale);
    Applied::Yes
}

fn first_number(value: &str) -> &str {
    value.split(',').next().unwrap_or("").trim()
}

fn unquote(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_owned()
    } else {
        value.to_owned()
    }
}

const KEY_NAMES: &[(&str, &str)] = &[
    ("arrow_left", "left"),
    ("arrow_right", "right"),
    ("arrow_up", "up"),
    ("arrow_down", "down"),
    ("bracket_left", "["),
    ("bracket_right", "]"),
    ("grave_accent", "`"),
    ("backquote", "`"),
    ("quote", "'"),
    ("equal", "="),
    ("minus", "-"),
    ("comma", ","),
    ("period", "."),
    ("slash", "/"),
    ("backslash", "\\"),
    ("semicolon", ";"),
    ("apostrophe", "'"),
    ("return", "enter"),
    ("page_up", "pageup"),
    ("page_down", "pagedown"),
    ("arrowleft", "left"),
    ("arrowright", "right"),
    ("arrowup", "up"),
    ("arrowdown", "down"),
    ("bracketleft", "["),
    ("bracketright", "]"),
];

const NAMED_KEYS: &[&str] = &[
    "backspace",
    "escape",
    "tab",
    "enter",
    "space",
    "up",
    "down",
    "left",
    "right",
    "pageup",
    "pagedown",
    "home",
    "end",
    "delete",
    "insert",
];

const MODIFIERS: &[(&str, &str)] = &[
    ("super", "cmd"),
    ("cmd", "cmd"),
    ("command", "cmd"),
    ("ctrl", "ctrl"),
    ("control", "ctrl"),
    ("alt", "alt"),
    ("opt", "alt"),
    ("option", "alt"),
    ("shift", "shift"),
];

const ACTIONS: &[(&str, &str)] = &[
    ("new_split:right", "split-right"),
    ("new_split:left", "split-left"),
    ("new_split:down", "split-down"),
    ("new_split:up", "split-up"),
    ("new_split:auto", "split-right"),
    ("goto_split:left", "pane-left"),
    ("goto_split:right", "pane-right"),
    ("goto_split:up", "pane-up"),
    ("goto_split:down", "pane-down"),
    ("goto_split:top", "pane-up"),
    ("goto_split:bottom", "pane-down"),
    ("goto_split:previous", "pane-prev"),
    ("goto_split:next", "pane-next"),
    ("resize_split:left", "resize-left"),
    ("resize_split:right", "resize-right"),
    ("resize_split:up", "resize-up"),
    ("resize_split:down", "resize-down"),
    ("toggle_split_zoom", "zoom"),
    ("equalize_splits", "equalize"),
    ("close_surface", "close"),
    ("close_tab", "close-tab"),
    ("close_window", "close-tab"),
    ("new_tab", "new"),
    ("new_window", "new-window"),
    ("previous_tab", "tab-prev"),
    ("next_tab", "tab-next"),
    ("last_tab", "tab-9"),
    ("prompt_surface_title", "rename"),
    ("prompt_tab_title", "rename"),
    ("toggle_fullscreen", "fullscreen"),
    ("toggle_maximize", "maximize"),
    ("increase_font_size", "font-up"),
    ("decrease_font_size", "font-down"),
    ("reset_font_size", "font-reset"),
    ("scroll_page_up", "scroll-page-up"),
    ("scroll_page_down", "scroll-page-down"),
    ("scroll_to_top", "scroll-top"),
    ("scroll_to_bottom", "scroll-bottom"),
    ("clear_screen", "clear"),
    ("start_search", "find"),
    ("search_selection", "find-selection"),
    ("navigate_search:next", "find-next"),
    ("navigate_search:previous", "find-prev"),
    ("end_search", "find-close"),
    ("open_config", "settings"),
    ("reload_config", "reload-config"),
    ("toggle_command_palette", "palette"),
    ("toggle_quick_terminal", "quick-terminal"),
    ("toggle_tab_overview", "goto"),
    ("select_all", "select-all"),
    ("paste_from_clipboard", "paste"),
    ("paste_from_selection", "paste-selection"),
    ("ignore", "ignore"),
    ("unbind", ""),
];

const WRITE_REGIONS: &[(&str, &str)] = &[
    ("write_screen_file", "screen"),
    ("write_scrollback_file", "scrollback"),
    ("write_selection_file", "selection"),
];

const BUILT_IN: &[(&str, &str)] = &[
    ("cmd+c", "copy_to_clipboard"),
    ("cmd+v", "paste_from_clipboard"),
    ("cmd+q", "quit"),
];

fn own<'a>(table: &'a [(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    table
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| *value)
}

fn key_name(part: &str) -> Option<String> {
    let mapped = own(KEY_NAMES, part)
        .map(str::to_owned)
        .or_else(|| strip_w3c_prefix(part))
        .unwrap_or_else(|| part.to_owned());
    if mapped.chars().count() == 1 || NAMED_KEYS.contains(&mapped.as_str()) || function_key(&mapped)
    {
        Some(mapped)
    } else {
        None
    }
}

fn strip_w3c_prefix(part: &str) -> Option<String> {
    for prefix in ["digit", "key"] {
        let Some(rest) = part.strip_prefix(prefix) else {
            continue;
        };
        let rest = rest.strip_prefix('_').unwrap_or(rest);
        if rest.chars().count() == 1 {
            return Some(rest.to_owned());
        }
    }
    None
}

fn function_key(key: &str) -> bool {
    let Some(number) = key.strip_prefix('f') else {
        return false;
    };
    number.parse::<u8>().is_ok_and(|value| {
        (1..=35).contains(&value) && !number.starts_with('0') && !number.is_empty()
    })
}

/// `super+shift+arrow_left` becomes `cmd+shift+left`.
#[must_use]
pub fn ghostty_chord(trigger: &str) -> Option<String> {
    let mut mods = HashSet::new();
    let mut key: Option<String> = None;
    for raw in trigger.to_lowercase().split('+') {
        let part = raw.trim().trim_start_matches("physical:");
        if part.is_empty() && raw.trim().is_empty() {
            return None;
        }
        if let Some(modifier) = own(MODIFIERS, part) {
            mods.insert(modifier);
            continue;
        }
        if key.is_some() || part.is_empty() {
            return None;
        }
        key = Some(part.to_owned());
    }
    let mut key = key?;
    if key.contains('>') {
        return None;
    }
    if key == "plus" {
        key = "=".to_owned();
        mods.insert("shift");
    }
    let key = key_name(&key)?;
    if mods.is_empty() && key.chars().count() == 1 {
        return None;
    }
    let mut parts = Vec::new();
    for modifier in ["cmd", "ctrl", "alt", "shift"] {
        if mods.contains(modifier) {
            parts.push(modifier);
        }
    }
    parts.push(key.as_str());
    Some(parts.join("+"))
}

/// The command a Ghostty action maps to.
#[must_use]
pub fn ghostty_action(action: &str) -> Option<String> {
    let trimmed = action.trim();
    if let Some(tab) = trimmed.strip_prefix("goto_tab:")
        && tab.len() == 1
        && tab.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Some(format!("tab-{tab}"));
    }
    let (name, argument) = trimmed.split_once(':').unwrap_or((trimmed, ""));
    let direction = argument.split(',').next().unwrap_or("");
    if let Some(command) =
        own(ACTIONS, &format!("{name}:{direction}")).or_else(|| own(ACTIONS, name))
    {
        return Some(command.to_owned());
    }
    parameterized(name, argument)
}

fn parameterized(name: &str, argument: &str) -> Option<String> {
    if name == "text" {
        return Some(format!("send:{}", zig_string(argument)));
    }
    if name == "esc" {
        return Some(format!("send:\u{1b}{}", zig_string(argument)));
    }
    if name == "copy_to_clipboard" {
        return ["", "plain", "mixed"]
            .contains(&argument.trim())
            .then(|| "copy".to_owned());
    }
    if let Some(region) = own(WRITE_REGIONS, name) {
        let mut parts = argument.split(',');
        let mode = parts.next().unwrap_or("").trim();
        let format = parts.next().unwrap_or("plain").trim();
        return (matches!(mode, "copy" | "paste" | "open") && format == "plain")
            .then(|| format!("write-file:{region}:{mode}"));
    }
    if argument.trim().is_empty() {
        return None;
    }
    let number = argument.trim().parse::<f64>().ok()?;
    if !number.is_finite() {
        return None;
    }
    if name == "set_font_size" {
        return Some(format!("font-size:{number}"));
    }
    if name == "scroll_page_lines" && number.fract() == 0.0 && number != 0.0 {
        return Some(format!("scroll-lines:{number}"));
    }
    if name == "move_tab" && number != 0.0 {
        return Some(if number < 0.0 {
            "tab-move-left".to_owned()
        } else {
            "tab-move-right".to_owned()
        });
    }
    if name == "jump_to_prompt" && number.fract() == 0.0 && number.is_finite() && number != 0.0 {
        return Some(if number == -1.0 {
            "prompt-prev".to_owned()
        } else if number == 1.0 {
            "prompt-next".to_owned()
        } else {
            format!("prompt-jump:{number}")
        });
    }
    None
}

/// Zig string-literal escapes. Unknown escapes stay literal. `\xNN` runs decode as UTF-8.
#[must_use]
pub fn zig_string(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\\' {
            let ch = value[index..].chars().next().expect("char");
            out.push(ch);
            index += ch.len_utf8();
            continue;
        }
        if let Some(decoded) = decode_hex_run(value, index) {
            out.push_str(&decoded.text);
            index = decoded.next;
            continue;
        }
        let rest = &value[index + 1..];
        if let Some(body) = rest.strip_prefix("u{")
            && let Some(end) = body.find('}')
        {
            let point = u32::from_str_radix(&body[..end], 16).ok();
            if point.is_some_and(|point| point <= 0x10_ffff)
                && let Some(ch) = char::from_u32(point.expect("checked"))
            {
                out.push(ch);
                index += 3 + end + 1;
                continue;
            }
        }
        let simple = rest.chars().next().and_then(|ch| match ch {
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            '\\' => Some('\\'),
            '\'' => Some('\''),
            '"' => Some('"'),
            _ => None,
        });
        if let Some(ch) = simple {
            out.push(ch);
            index += 2;
            continue;
        }
        out.push('\\');
        index += 1;
    }
    out
}

struct HexRun {
    text: String,
    next: usize,
}

fn decode_hex_run(value: &str, start: usize) -> Option<HexRun> {
    let bytes = value.as_bytes();
    if start + 3 >= bytes.len() || bytes[start] != b'\\' || bytes[start + 1] != b'x' {
        return None;
    }
    let mut raw = Vec::new();
    let mut index = start;
    while index + 3 < bytes.len()
        && bytes[index] == b'\\'
        && bytes[index + 1] == b'x'
        && bytes[index + 2].is_ascii_hexdigit()
        && bytes[index + 3].is_ascii_hexdigit()
    {
        let pair = &value[index + 2..index + 4];
        raw.push(u8::from_str_radix(pair, 16).ok()?);
        index += 4;
    }
    if raw.is_empty() {
        return None;
    }
    Some(HexRun {
        text: String::from_utf8_lossy(&raw).into_owned(),
        next: index,
    })
}

fn trigger_end(value: &str) -> Option<usize> {
    let first = value.find('=')?;
    if first > 0 && value.as_bytes().get(first - 1) == Some(&b'+') {
        value[first + 1..].find('=').map(|next| first + 1 + next)
    } else {
        Some(first)
    }
}

/// Apply one `keybind` value. `false` when it lacks the `trigger=action` shape.
fn set_keybind(config: &mut GhosttyConfig, value: &str, origin: &Origin) -> bool {
    if value == "clear" {
        config.keybinds.clear();
        config.globals.clear();
        config.unmapped.clear();
        config.binds.clear();
        return true;
    }
    let Some(equals) = trigger_end(value) else {
        return false;
    };
    let raw = &value[..equals];
    let global = is_global_prefix(raw);
    let trigger = strip_prefixes(raw);
    let action = value[equals + 1..].trim();
    let chord = ghostty_chord(trigger);
    if chord
        .as_deref()
        .is_some_and(|chord| own(BUILT_IN, chord) == Some(action))
    {
        let chord = chord.expect("checked");
        forget_chord(config, &chord);
        return true;
    }
    let command = ghostty_action(action);
    let Some(chord) = chord else {
        config.unmapped.push(format!("{trigger} = {action}"));
        return true;
    };
    let Some(command) = command else {
        forget_chord(config, &chord);
        config.unmapped.push(format!("{trigger} = {action}"));
        return true;
    };
    config.binds.push(Bind {
        chord: chord.clone(),
        command: command.clone(),
        origin: origin.clone(),
    });
    config.keybinds.insert(chord.clone(), command);
    if global {
        if !config.globals.contains(&chord) {
            config.globals.push(chord);
        }
    } else {
        config.globals.retain(|item| item != &chord);
    }
    true
}

/// A later line returned `chord` to the built-in, so earlier binds stop applying.
fn forget_chord(config: &mut GhosttyConfig, chord: &str) {
    config.keybinds.remove(chord);
    config.globals.retain(|item| item != chord);
    config.binds.retain(|bind| bind.chord != chord);
}

fn is_global_prefix(raw: &str) -> bool {
    let mut rest = raw;
    loop {
        if rest.starts_with("global:") {
            return true;
        }
        let next = ["all:", "unconsumed:", "performable:"]
            .iter()
            .find_map(|prefix| rest.strip_prefix(prefix));
        match next {
            Some(next) => rest = next,
            None => return false,
        }
    }
}

struct Prefix<'a> {
    rest: &'a str,
}

fn strip_one_prefix(value: &str) -> Option<Prefix<'_>> {
    let (name, rest) = value.split_once(':')?;
    if matches!(name, "global" | "all" | "unconsumed" | "performable") {
        Some(Prefix { rest })
    } else {
        None
    }
}

fn strip_prefixes(value: &str) -> &str {
    let mut rest = value;
    while let Some(stripped) = strip_one_prefix(rest) {
        rest = stripped.rest;
    }
    rest
}

/// A theme from Ghostty's colours, or `None` when it names neither default colour.
#[must_use]
pub fn ghostty_theme(config: &GhosttyConfig) -> Option<Theme> {
    let background = config.background.clone()?;
    let foreground = config.foreground.clone()?;
    let palette = config.palette.iter().cloned().collect::<Option<Vec<_>>>();
    Some(Theme {
        id: "ghostty".to_owned(),
        name: "Ghostty".to_owned(),
        appearance: if linear_luminance(&background) < 0.4 {
            "dark".to_owned()
        } else {
            "light".to_owned()
        },
        surface: mix(&background, &foreground, 0.035),
        accent: config
            .cursor
            .clone()
            .or_else(|| config.palette[4].clone())
            .unwrap_or_else(|| mix(&background, &foreground, 0.7)),
        cursor: config.cursor.clone().unwrap_or_else(|| foreground.clone()),
        selection: config
            .selection_background
            .clone()
            .unwrap_or_else(|| mix(&background, &foreground, 0.2)),
        danger: config.palette[1]
            .clone()
            .unwrap_or_else(|| "#e45f57".to_owned()),
        warning: config.palette[3]
            .clone()
            .unwrap_or_else(|| "#d6a84f".to_owned()),
        success: config.palette[2]
            .clone()
            .unwrap_or_else(|| "#7fae8b".to_owned()),
        palette,
        selection_foreground: config.selection_foreground.clone(),
        divider: config.split_divider_color.clone(),
        background,
        foreground,
    })
}

fn linear_luminance(color: &str) -> f64 {
    let body = color.trim_start_matches('#');
    let Ok(value) = u32::from_str_radix(&body[..6.min(body.len())], 16) else {
        return 0.0;
    };
    let channel = |shift: u32| f64::from((value >> shift) & 0xff) / 255.0;
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

/// `base` with every field the Ghostty config specifies. Saved prefs still win
/// after the first launch; call this only to seed or to apply explicitly.
#[must_use]
pub fn ghostty_prefs(base: &DisplayPrefs, config: &GhosttyConfig) -> DisplayPrefs {
    let mut next = base.clone();
    next.ghostty_keys = true;
    if let Some(family) = &config.font_family {
        next.font_family.clone_from(family);
    }
    if let Some(size) = config.font_size {
        next.font_size = size.round();
    }
    if config.font_family.is_some() || config.font_size.is_some() || config.cell_height.is_some() {
        next.line_height = 1.0;
    }
    if let Some(width) = config.cell_width {
        next.cell_width = width;
    }
    if let Some(height) = config.cell_height {
        next.cell_height = height;
    }
    if let Some(padding) = config.padding_x {
        next.padding_x = padding + 2.0;
    }
    if let Some(padding) = config.padding_y {
        next.padding_y = padding + 2.0;
    }
    if let Some(opacity) = config.unfocused_split_opacity {
        next.unfocused_opacity = opacity;
    }
    if let Some(option) = config.option_as_alt {
        next.option_as_alt = option;
    }
    if ghostty_theme(config).is_some() {
        next.theme_id = "ghostty".to_owned();
    }
    next
}

/// The notice a reload posts when malformed lines were skipped, as
/// `(title, body)`. `None` when every line parsed; values Ghostty accepts but
/// this app cannot use are listed in Settings without a notice.
#[must_use]
pub fn skipped_lines_notice(config: &GhosttyConfig) -> Option<(String, String)> {
    let first = config.diagnostics.first()?;
    let skipped = config.diagnostics.len();
    let body = format!(
        "{}: {} ({}). Settings > Ghostty integration lists {}.",
        first.origin.label(),
        first.text,
        first.reason,
        if skipped == 1 { "it" } else { "all of them" },
    );
    let title = if skipped == 1 {
        "Skipped 1 Ghostty config line".to_owned()
    } else {
        format!("Skipped {skipped} Ghostty config lines")
    };
    Some((title, body))
}

/// Read the user's Ghostty config files, theme file first, from disk each call
/// so an external edit shows on the next reload. `None` when Ghostty is not configured.
#[must_use]
pub fn read_ghostty_sources() -> Option<Vec<GhosttySource>> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| home.join(".config"));
    read_ghostty_sources_in(&home, &config_home)
}

/// [`read_ghostty_sources`] for an explicit home and XDG config directory.
#[must_use]
pub fn read_ghostty_sources_in(home: &Path, config_home: &Path) -> Option<Vec<GhosttySource>> {
    let configs = [
        config_home.join("ghostty/config"),
        config_home.join("ghostty/config.ghostty"),
        home.join("Library/Application Support/com.mitchellh.ghostty/config"),
        home.join("Library/Application Support/com.mitchellh.ghostty/config.ghostty"),
    ];
    let main = configs.iter().find(|path| path.is_file())?;
    let text = read_text(main)?;
    let mut files = vec![GhosttySource {
        path: Some(main.clone()),
        text,
    }];
    files.extend(include_texts(main, &files[0].text, home));
    let all = files
        .iter()
        .map(|source| source.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut sources: Vec<GhosttySource> = theme_text(&all, config_home).into_iter().collect();
    sources.extend(files);
    Some(sources)
}

fn include_texts(main: &Path, text: &str, home: &Path) -> Vec<GhosttySource> {
    let mut found = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("config-file") else {
            continue;
        };
        let Some(rest) = rest.trim().strip_prefix('=') else {
            continue;
        };
        let value = unquote(rest.trim());
        let value = value.trim_start_matches('?').trim();
        if value.is_empty() {
            continue;
        }
        let path = if Path::new(value).is_absolute() {
            PathBuf::from(value)
        } else if let Some(rest) = value.strip_prefix("~/") {
            home.join(rest)
        } else {
            main.parent().unwrap_or(Path::new(".")).join(value)
        };
        if let Some(text) = read_text(&path) {
            found.push(GhosttySource {
                path: Some(path),
                text,
            });
        }
    }
    found
}

fn theme_text(config: &str, config_home: &Path) -> Option<GhosttySource> {
    let mut name = None;
    for line in config.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("theme") else {
            continue;
        };
        let Some(rest) = rest.trim().strip_prefix('=') else {
            continue;
        };
        name = Some(unquote(rest.trim()));
    }
    let value = name?;
    let dark = value.split(',').find_map(|part| {
        let part = part.trim();
        part.strip_prefix("dark:").map(str::trim)
    });
    let plain = (!value.contains(':')).then_some(value.as_str());
    let name = dark.or(plain)?.trim();
    if name.is_empty() {
        return None;
    }
    let candidates = if Path::new(name).is_absolute() {
        vec![PathBuf::from(name)]
    } else {
        vec![
            config_home.join("ghostty/themes").join(name),
            PathBuf::from("/Applications/Ghostty.app/Contents/Resources/ghostty/themes").join(name),
        ]
    };
    candidates.into_iter().find_map(|path| {
        read_text(&path).map(|text| GhosttySource {
            path: Some(path),
            text,
        })
    })
}

fn read_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::super::display::default_display;
    use super::super::keymap::{KeyChord, key_chord};
    use super::{
        GhosttySource, ghostty_action, ghostty_chord, ghostty_prefs, ghostty_theme, parse_ghostty,
        parse_ghostty_sources, read_ghostty_sources_in, skipped_lines_notice, zig_string,
    };

    const CONFIG: &str = r#"
font-family = "JetBrainsMono Nerd Font"
font-family = Symbols Nerd Font
font-size = 14
adjust-cell-width = -8%
adjust-cell-height = 2
background = #071012
foreground = c5d0cd
cursor-color = #45d0bd
selection-background = #1d3035
palette = 0=#081114
palette = 1=#e45f57
palette = 17=#ffffff
window-padding-x = 4
window-padding-y = 4,6
unfocused-split-opacity = 0.85
split-divider-color = #1d3035
macos-option-as-alt = left
keybind = global:ctrl+grave_accent=toggle_quick_terminal
keybind = super+d=new_split:right
keybind = super+ctrl+arrow_left=goto_split:left
keybind = super+alt+arrow_up=resize_split:up,20
keybind = super+plus=increase_font_size:1
keybind = super+3=goto_tab:3
keybind = ctrl+shift+tab=previous_tab
keybind = super+shift+arrow_up=jump_to_prompt:-1
keybind = super+shift+arrow_down=jump_to_prompt:3
keybind = ctrl+shift+c=copy_to_clipboard:mixed
keybind = ctrl+shift+v=paste_from_clipboard
keybind = super+alt+a=select_all
keybind = super+alt+s=write_scrollback_file:open
keybind = super+alt+v=write_selection_file:copy,html
keybind = super+t=unbind
keybind = shift+enter=text:\x1b\r
keybind = super+alt+t=text:a=b
keybind = super+==reset_font_size
keybind = ctrl+a>c=new_tab
keybind = x=new_tab
keybind = super+c=copy_to_clipboard
keybind = super+shift+left=move_tab:-1
keybind = super+i=ignore
keybind = super+e=search_selection
keybind = super+alt+h=new_split:left
keybind = super+k=reset
keybind = f12=set_font_size:15
"#;

    #[test]
    fn parses_font_colours_and_keybinds() {
        let config = parse_ghostty(CONFIG);
        assert_eq!(
            config.font_family.as_deref(),
            Some("JetBrainsMono Nerd Font")
        );
        assert_eq!(config.font_size, Some(14.0));
        assert!((config.cell_width.unwrap() - 0.92).abs() < 1e-9);
        assert!(config.cell_height.is_none());
        assert_eq!(config.background.as_deref(), Some("#071012"));
        assert_eq!(config.foreground.as_deref(), Some("#c5d0cd"));
        assert_eq!(config.palette[0].as_deref(), Some("#081114"));
        assert!(config.palette[2].is_none());
        assert_eq!(config.padding_y, Some(4.0));
        assert_eq!(
            config.keybinds.get("cmd+d").map(String::as_str),
            Some("split-right")
        );
        assert_eq!(
            config.keybinds.get("ctrl+`").map(String::as_str),
            Some("quick-terminal")
        );
        assert_eq!(config.keybinds.get("cmd+t").map(String::as_str), Some(""));
        assert!(config.globals.iter().any(|chord| chord == "ctrl+`"));
        assert!(!config.keybinds.contains_key("cmd+c"));
        assert_eq!(
            config.keybinds.get("shift+enter").map(String::as_str),
            Some("send:\u{1b}\r")
        );
        assert_eq!(
            config.keybinds.get("f12").map(String::as_str),
            Some("font-size:15")
        );
        assert!(
            config
                .unmapped
                .iter()
                .any(|line| line.contains("super+k = reset"))
        );
        let theme = ghostty_theme(&config).expect("theme");
        assert_eq!(theme.accent, "#45d0bd");
        assert!(theme.palette.is_none());
        let prefs = ghostty_prefs(&default_display(), &config);
        assert_eq!(prefs.line_height, 1.0);
        assert_eq!(prefs.padding_x, 6.0);
        assert_eq!(prefs.theme_id, "ghostty");
        assert_eq!(
            ghostty_chord("shift+super+bracket_right").as_deref(),
            Some("cmd+shift+]")
        );
        assert_eq!(ghostty_chord("ctrl+a>c"), None);
        assert_eq!(
            ghostty_action("jump_to_prompt:1").as_deref(),
            Some("prompt-next")
        );
        assert_eq!(ghostty_action("csi:A"), None);
        assert_eq!(zig_string(r"\xe2\x82\xac"), "€");
        let mut event = KeyChord::new("tab");
        event.ctrl = true;
        assert_eq!(key_chord(&event), "ctrl+tab");
        assert_eq!(key_chord(&KeyChord::new("a")), "");
    }

    #[test]
    fn malformed_lines_are_reported_and_the_rest_still_apply() {
        let config = parse_ghostty(
            "font-size = 14\nthis line has no equals\nbackground = #zzzzzz\n\
             font-size = big\nkeybind = nonsense\nforeground = #c5d0cd\r\nbackground = #071012\n",
        );
        assert_eq!(config.font_size, Some(14.0));
        assert_eq!(config.foreground.as_deref(), Some("#c5d0cd"));
        assert_eq!(config.background.as_deref(), Some("#071012"));
        let skipped: Vec<(usize, &str)> = config
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.origin.line, diagnostic.text.as_str()))
            .collect();
        assert_eq!(
            skipped,
            vec![
                (2, "this line has no equals"),
                (3, "background = #zzzzzz"),
                (4, "font-size = big"),
                (5, "keybind = nonsense"),
            ]
        );
        assert_eq!(config.origins["font-size"].line, 1);
        assert_eq!(config.origins["background"].line, 7);
        assert!(ghostty_theme(&config).is_some());
    }

    #[test]
    fn valid_ghostty_values_this_app_cannot_use_are_not_malformed() {
        let config = parse_ghostty(
            "palette = 200=#ffffff\nadjust-cell-height = 2\nadjust-cell-width = -1\n\
             background = black\nfont-size = 900\nadjust-cell-height = 300%\n\
             unfocused-split-opacity = 0.1\npalette = 3=#00ff00\n",
        );
        assert!(config.diagnostics.is_empty(), "{:?}", config.diagnostics);
        let unsupported: Vec<usize> = config
            .unsupported
            .iter()
            .map(|diagnostic| diagnostic.origin.line)
            .collect();
        assert_eq!(unsupported, vec![1, 2, 3, 4, 5, 6]);
        assert!(config.unsupported[0].reason.contains("palette slots"));
        assert!(config.unsupported[3].reason.contains("hex"));
        // Ghostty clamps the opacity, so the app does too.
        assert_eq!(config.unfocused_split_opacity, Some(0.15));
        assert_eq!(config.origins["unfocused-split-opacity"].line, 7);
        assert_eq!(config.palette[3].as_deref(), Some("#00ff00"));
        assert!(config.cell_height.is_none());
        assert!(config.background.is_none());

        let broken = parse_ghostty("palette = 300=#ffffff\nadjust-cell-height = tall\n");
        assert_eq!(broken.diagnostics.len(), 2);
        assert!(broken.unsupported.is_empty());
        assert_eq!(skipped_lines_notice(&config), None);
        let (title, body) = skipped_lines_notice(&broken).expect("notice");
        assert_eq!(title, "Skipped 2 Ghostty config lines");
        assert!(body.starts_with("line 1: palette = 300=#ffffff (value is malformed)"));
        assert!(body.ends_with("lists all of them."));
    }

    #[test]
    fn origins_name_the_file_and_line_across_sources() {
        let theme = GhosttySource {
            path: Some("/themes/night".into()),
            text: "background = #000000\nforeground = #ffffff\n".to_owned(),
        };
        let main = GhosttySource {
            path: Some("/cfg/config".into()),
            text: "# mine\n\nfont-family = Iosevka\nbackground = #101010\nkeybind = super+d=new_split:down\n"
                .to_owned(),
        };
        let config = parse_ghostty_sources(&[theme, main]);
        assert_eq!(config.origins["foreground"].label(), "/themes/night:2");
        assert_eq!(config.origins["background"].label(), "/cfg/config:4");
        assert_eq!(config.origins["font-family"].label(), "/cfg/config:3");
        assert_eq!(config.binds.len(), 1);
        assert_eq!(config.binds[0].origin.label(), "/cfg/config:5");
    }

    #[test]
    fn reload_reads_an_external_edit() {
        let root = std::env::temp_dir().join(format!(
            "phux-ghostty-reload-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let config_home = root.join("xdg");
        let dir = config_home.join("ghostty");
        std::fs::create_dir_all(dir.join("themes")).expect("config dir");
        let file = dir.join("config");
        std::fs::write(dir.join("themes/night"), "background = #000000\n").expect("theme");
        std::fs::write(
            &file,
            "theme = night\nfont-size = 12\nconfig-file = extra\n",
        )
        .expect("write");
        std::fs::write(dir.join("extra"), "keybind = super+d=new_split:down\n").expect("include");
        let first = parse_ghostty_sources(
            &read_ghostty_sources_in(&root, &config_home).expect("config found"),
        );
        assert_eq!(first.font_size, Some(12.0));
        assert_eq!(first.background.as_deref(), Some("#000000"));
        assert_eq!(
            first.origins["background"].file.as_deref(),
            Some(dir.join("themes/night").as_path())
        );
        assert_eq!(
            first.binds[0].origin.file.as_deref(),
            Some(dir.join("extra").as_path())
        );

        // Another editor rewrites the file while the app runs.
        std::fs::write(&file, "font-size = 16\nbroken\n").expect("rewrite");
        let second = parse_ghostty_sources(
            &read_ghostty_sources_in(&root, &config_home).expect("config found"),
        );
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(second.font_size, Some(16.0));
        assert_eq!(
            second.origins["font-size"].file.as_deref(),
            Some(file.as_path())
        );
        assert!(second.background.is_none());
        assert!(second.binds.is_empty());
        assert_eq!(second.diagnostics.len(), 1);
        assert_eq!(second.diagnostics[0].origin.line, 2);
        // The reload notice names the skipped line; the first read had none.
        assert_eq!(skipped_lines_notice(&first), None);
        let (title, body) = skipped_lines_notice(&second).expect("notice");
        assert_eq!(title, "Skipped 1 Ghostty config line");
        assert!(
            body.ends_with(
                ":2: broken (expected key = value). Settings > Ghostty integration lists it."
            ),
            "{body}"
        );
    }
}
