//! Settings page (ADR-0101): a schema-driven editor of the user's
//! `config.toml`, built on ratcn.
//!
//! Each row is one key with its effective value, origin layer, description, and
//! when a change applies; each edit sets or removes one leaf
//! (`phux_config::settings::write_edit`) and then reloads.
//!
//! The sections are a ratcn [`Tabs`] row, the setting rows a [`ScrollArea`].
//! A row's value is the control its kind wants: a [`Checkbox`] for a bool, a
//! [`Cycle`] for a choice or a tri-state, and, while one is being edited, an
//! [`Input`] in place of the value text. A click on a tab, a box, or a cycle
//! acts on it; a click elsewhere on a row selects it.
//!
//! The keyboard stays the page's own: ratcn's controls answer `h`/`l` and
//! `C-n`/`C-p`, but here letters are always filter text, so no query can
//! write the file, and Tab steps sections. Keys reach ratcn only while the
//! value editor is open, where its [`Input`] is the line editor. A key set by
//! an `extends` layer refuses a reset, naming that file.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use phux_config::LayerSource;
use phux_config::settings::{
    Applies, CATALOG, Edit, SettingKind, SettingSection, SettingSpec, SettingsSnapshot,
    render_value,
};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratcn::runtime::{
    Component, DeclareCtx, Event, EventResult, FocusState, PaintCtx, Ratcn, ScopeOptions,
};
use ratcn::{
    Checkbox, CheckboxStyle, Cycle, Input, InputState, ScrollArea, ScrollAreaStyle, Tab, Tabs,
    TabsActivation,
};

use super::select_list::{WHEEL_SCROLL_ROWS, fuzzy_rank, fuzzy_score};
use super::widgets::{
    MODAL_PAD, Modal, centered_panel, footer_status_budget, modal_inner_width, scroll_into_view,
    wrap_words,
};
use super::{OverlayCommand, RenderOverlay};
use crate::render::kit::{self, Surface};
use crate::render::theme::SLOT_SPECS;
use crate::render::{ChromeBreakpoints, Theme, clip_left, clip_text, display_width, home_to_tilde};

mod edit;
mod keys;
mod parse;
mod render;
mod surface;

use parse::{display_path_for, parse_input};
use surface::{Control, Layout, Model, Msg, RowDecl};

/// Rows the detail panel takes on a roomy viewport (rule excluded): key
/// and kind, summary, two lines of detail, the default/origin/applies
/// facts, and the outcome line.
const DETAIL_ROWS: u16 = 6;
/// Rows the detail panel keeps on a row-starved viewport.
const DETAIL_ROWS_COMPACT: u16 = 2;
/// Rows of the modal box that are not list rows: the two borders, the
/// query line, the section tabs, and the rule above the detail panel.
const FIXED_ROWS: u16 = 5;

/// The key legends inset into the bottom border while browsing, longest
/// first; the longest one that clears the position text is painted.
const SETTINGS_KEYS: [&str; 3] = [
    "⏎ edit · ←→ step · tab section · del reset · esc",
    "⏎ edit · tab section · esc",
    "⏎ edit · esc",
];
/// The legend while the value editor is open.
const EDITOR_KEYS: &str = "⏎ save · esc";
/// The longest browsing legend that leaves `status` its room in a modal
/// `modal_width` cells wide, else the shortest.
fn settings_legend(modal_width: u16, status: &str) -> &'static str {
    SETTINGS_KEYS
        .into_iter()
        .find(|keys| display_width(status) <= footer_status_budget(modal_width, keys))
        .unwrap_or(SETTINGS_KEYS[SETTINGS_KEYS.len() - 1])
}
/// Width of the origin badge column, including its leading gap.
const BADGE_COLS: usize = 9;
/// The checkbox slot: the marker, a space, and the wider label (`false`),
/// so the box holds still as it flips.
const TOGGLE_COLS: usize = 7;
/// A tri-state's [`Cycle`] options, in stepping order.
const TRI_STATE: [&str; 3] = ["unset", "true", "false"];

/// One edit the user made through this page, kept so `C-z` can put the
/// file back the way it was.
#[derive(Debug, Clone, PartialEq)]
struct Undo {
    key: &'static str,
    restore: Edit,
}

/// The outcome line under the detail panel.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    /// The file was written; the text is what changed.
    Saved(String),
    /// Nothing was written; the text says why.
    Refused(String),
    /// A neutral note (an undo with nothing to undo, a no-op reset).
    Note(String),
}

/// The settings page overlay.
pub struct SettingsOverlay {
    /// The user's config file: read for the snapshot, written by edits.
    path: PathBuf,
    /// The path as the header shows it (`~` for the home directory).
    display_path: String,
    /// The resolved layer stack, or why the file does not load.
    snapshot: Result<SettingsSnapshot, String>,
    /// Every setting the page offers: the schema catalogue plus the
    /// renderer's theme slots, in section order.
    specs: Vec<&'static SettingSpec>,
    /// The section shown while the query is empty.
    section: usize,
    /// The filter text.
    query: String,
    /// Selection index into the visible rows.
    selected: usize,
    /// Rows the list viewport held at the last paint.
    page: Cell<usize>,
    /// The key the value editor is open on; its text is `model.input`.
    editor: Option<&'static str>,
    /// The last outcome line.
    status: Option<Status>,
    /// Edits made through this page, newest last.
    undo: Vec<Undo>,
    /// Color slots snapshotted from the active [`Theme`]; refreshed by
    /// [`RenderOverlay::set_theme`] after a reload so a theme edit shows
    /// on the page that made it.
    theme: Theme,
    /// `[chrome]` thresholds, stamped by `OverlayState::push`.
    breakpoints: ChromeBreakpoints,
    /// What the ratcn surface reads.
    model: Model,
    ui: Surface<Model, Msg>,
    /// The viewport of the last paint, which input is laid out against.
    outer: Cell<Rect>,
}

impl std::fmt::Debug for SettingsOverlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsOverlay")
            .field("path", &self.path)
            .field("loaded", &self.snapshot.is_ok())
            .field("section", &SettingSection::ALL[self.section])
            .field("query", &self.query)
            .field("selected", &self.selected)
            .field("editor", &self.editor)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// Read the config file at `path` and resolve the layer stack; a missing
/// file loads as empty, any other failure is the message the page shows.
fn load_snapshot(path: &std::path::Path) -> Result<SettingsSnapshot, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("cannot read {}: {err}", path.display())),
    };
    SettingsSnapshot::load(&text, path).map_err(|err| err.to_string())
}

impl SettingsOverlay {
    /// Open the page over the config file at `path`; every edit re-reads the
    /// file, so the page shows what is on disk.
    #[must_use]
    pub fn open(path: PathBuf, theme: &Theme) -> Self {
        let mut specs: Vec<&'static SettingSpec> = Vec::new();
        for section in SettingSection::ALL {
            specs.extend(CATALOG.iter().filter(|s| s.section == *section));
            specs.extend(SLOT_SPECS.iter().filter(|s| s.section == *section));
        }
        let home = std::env::var("HOME").ok();
        let display_path = display_path_for(&path, home.as_deref());
        let snapshot = load_snapshot(&path);
        let runtime = Ratcn::new().focus(|m: &Model| &m.focus, Msg::Focus);
        let mut page = Self {
            path,
            display_path,
            snapshot,
            specs,
            section: 0,
            query: String::new(),
            selected: 0,
            page: Cell::new(0),
            editor: None,
            status: None,
            undo: Vec::new(),
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
            model: Model::default(),
            ui: Surface::new(runtime),
            outer: Cell::new(Rect::new(0, 0, 80, 24)),
        };
        page.sync_focus();
        page
    }

    /// Show `text` in the header instead of the real path (tests: the temp
    /// dir varies per run).
    #[cfg(test)]
    fn with_display_path(mut self, text: &str) -> Self {
        self.display_path = text.to_owned();
        self
    }

    /// Put the selection on `key` in its section (tests).
    #[cfg(test)]
    fn focus(&mut self, key: &str) {
        let spec = self
            .specs
            .iter()
            .copied()
            .find(|s| s.key == key)
            .unwrap_or_else(|| panic!("{key} is not a settings row"));
        let section = SettingSection::ALL
            .iter()
            .position(|s| *s == spec.section)
            .expect("section listed");
        self.select_section(section);
        self.selected = self
            .visible_specs()
            .iter()
            .position(|s| s.key == key)
            .expect("row visible");
        self.sync_focus();
    }

    /// The value editor's text, when it is open (tests).
    #[cfg(test)]
    fn editor_text(&self) -> Option<&str> {
        self.editor.map(|_| self.model.input.value())
    }

    // ---- data -----------------------------------------------------------

    /// Re-read the file and resolve the layer stack.
    fn reload_snapshot(&mut self) {
        self.snapshot = load_snapshot(&self.path);
    }

    /// The settings the list shows: the current section's rows while the
    /// query is empty, otherwise every setting the query fuzzy-matches,
    /// best first. Layout-independent, so `selected` always indexes this.
    fn visible_specs(&self) -> Vec<&'static SettingSpec> {
        if self.query.is_empty() {
            let section = SettingSection::ALL[self.section];
            return self
                .specs
                .iter()
                .copied()
                .filter(|s| s.section == section)
                .collect();
        }
        let q = self.query.to_lowercase();
        let hays = self
            .specs
            .iter()
            .enumerate()
            .map(|(i, spec)| (i, format!("{} {}", spec.key, spec.summary)));
        fuzzy_rank(&q, hays)
            .into_iter()
            .map(|i| self.specs[i])
            .collect()
    }

    /// How many settings match the query in `section` (the section
    /// column's count while filtering).
    fn matches_in(&self, section: SettingSection) -> usize {
        let q = self.query.to_lowercase();
        self.specs
            .iter()
            .filter(|s| s.section == section)
            .filter(|s| {
                fuzzy_score(&q, &format!("{} {}", s.key, s.summary).to_lowercase()).is_some()
            })
            .count()
    }

    /// The selected setting, if any row is visible.
    fn selected_spec(&self) -> Option<&'static SettingSpec> {
        self.visible_specs().get(self.selected).copied()
    }

    /// The effective value at `key`, or `None` when unset (or the file
    /// does not load).
    fn value_at(&self, key: &str) -> Option<toml::Value> {
        self.snapshot.as_ref().ok()?.value_at(key).cloned()
    }

    /// The value the setting behaves as: the merged value, or a theme slot's
    /// renderer default.
    fn effective(&self, spec: &SettingSpec) -> Option<toml::Value> {
        self.value_at(spec.key).or_else(|| {
            (spec.section == SettingSection::Theme)
                .then(|| self.default_at(spec))
                .flatten()
        })
    }

    /// The shipped default at `key`: the catalogue's from the schema, a
    /// theme slot's from the default palette.
    fn default_at(&self, spec: &SettingSpec) -> Option<toml::Value> {
        if spec.section == SettingSection::Theme {
            return Theme::default()
                .slot(spec.leaf())
                .map(|color| toml::Value::String(crate::render::theme::color_to_string(color)));
        }
        self.snapshot.as_ref().ok()?.default_at(spec.key).cloned()
    }

    /// The layer above the defaults that set `key`, if any.
    fn origin_at(&self, key: &str) -> Option<LayerSource> {
        self.snapshot.as_ref().ok()?.origin_at(key).cloned()
    }

    /// Count of settings in `section` that a layer overrides.
    fn overridden_in(&self, section: SettingSection) -> usize {
        self.specs
            .iter()
            .filter(|s| s.section == section && self.origin_at(s.key).is_some())
            .count()
    }

    // ---- selection ------------------------------------------------------

    /// Keep `selected` inside the visible list (or at 0 when it is empty).
    const fn clamp_selection(&mut self, len: usize) {
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }

    const fn select_down(&mut self, len: usize) {
        if self.selected + 1 < len {
            self.selected += 1;
        }
    }

    const fn select_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn page_rows(&self) -> usize {
        self.page.get().max(1)
    }

    fn select_section(&mut self, index: usize) {
        self.section = index % SettingSection::ALL.len();
        self.query.clear();
        self.selected = 0;
        self.model.scroll.set(0);
    }

    fn requery(&mut self) {
        self.selected = 0;
        self.model.scroll.set(0);
    }

    /// Escape clears a live query first; with no query it closes the overlay.
    fn clear_query_or_dismiss(&mut self) -> OverlayCommand {
        if self.query.is_empty() {
            return OverlayCommand::Dismiss;
        }
        self.query.clear();
        self.requery();
        OverlayCommand::Stay
    }

    /// Tab / Shift-Tab: the next or previous section, wrapping.
    fn step_section(&mut self, backwards: bool) {
        let count = SettingSection::ALL.len();
        let next = if backwards {
            (self.section + count - 1) % count
        } else {
            (self.section + 1) % count
        };
        self.select_section(next);
    }

    /// Append printable key text to the filter query.
    fn type_filter_text(&mut self, text: Option<&str>) {
        let Some(text) = text.filter(|t| !t.chars().any(char::is_control)) else {
            return;
        };
        self.query.push_str(text);
        self.status = None;
        self.requery();
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests;
