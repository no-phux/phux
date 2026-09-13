//! Settings page (phux-u1tq.4): browse, search, edit, and reset every
//! setting without leaving the terminal.
//!
//! The page is a file editor with a schema, not a runtime knob panel
//! (ADR-0101). Every row is one key of the user's `config.toml`: its
//! effective value, the layer that set it, what it does, and when a change
//! takes effect. Every edit is one leaf set or removed in that file --
//! comments and formatting preserved, validated before anything touches
//! disk (`phux_config::settings::write_edit`) -- followed by the same
//! atomic reload `reload-config` performs. Nothing here writes running
//! state back: toggling the sidebar with `prefix-b` never lands in the
//! file, editing `sidebar.enabled` on this page does.
//!
//! ## Layout
//!
//! A roomy viewport shows three regions inside one modal: a section column
//! on the left, the selected section's rows on the right (marker, key,
//! value, origin badge), and a detail panel beneath with the description,
//! the shipped default, the allowed values, and when a change applies. A
//! column-starved viewport drops the section column and shows section
//! headers inline; a row-starved one shrinks the detail panel first.
//!
//! ## Input model
//!
//! Browse mode mirrors the palette: printable text filters across every
//! section (the section column then shows match counts), `j`/`k` navigate
//! while the query is empty, and Esc first clears the query, then closes.
//! `Tab` / `Shift-Tab` step through sections. On the selected row, Enter
//! and Space toggle a bool, cycle a choice, or open the inline editor;
//! Left/Right cycle a choice or step an integer (only the arrows: a letter
//! is always filter text, so no keystroke of a query can write the file). `Delete` (or `C-r`) resets
//! an overridden key to the shipped default by removing it from the file;
//! `C-z` undoes the last edit. The inline editor commits on Enter and
//! cancels on Esc.
//!
//! Only the user's own file is ever written. A key set by an `extends`
//! layer shows that layer's name as its origin and refuses a reset with a
//! message naming the file, because removing it here could not change what
//! that layer says.

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
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::select_list::{WHEEL_SCROLL_ROWS, fuzzy_score};
use super::widgets::{Modal, centered_panel, paint_scrollbar, scroll_into_view};
use super::{OverlayCommand, RenderOverlay};
use crate::render::theme::SLOT_SPECS;
use crate::render::{ChromeBreakpoints, Theme, clip_text, display_width};

/// Columns of the section column: a marker, the longest title
/// (`Experimental`), a gap, and a two-digit count.
const SECTION_COL: u16 = 17;
/// The rule between the section column and the rows, with its gaps.
const SECTION_RULE: &str = " \u{2502} ";
/// Interior width below which the section column is dropped even on a
/// viewport the breakpoints call roomy: the rows need the room more.
const TWO_COLUMN_MIN_WIDTH: u16 = 64;
/// Rows the detail panel takes on a roomy viewport (rule excluded): key
/// and kind, summary, two lines of detail, the default/origin/applies
/// facts, and the outcome line.
const DETAIL_ROWS: u16 = 6;
/// Rows the detail panel keeps on a row-starved viewport.
const DETAIL_ROWS_COMPACT: u16 = 2;
/// Rows of the modal box that are not list rows on a roomy layout: the two
/// borders, the query line and its blank, the footer and its blank, and the
/// rule above the detail panel.
const FIXED_ROWS: u16 = 7;
/// Width of the origin badge column, including its leading gap.
const BADGE_COLS: usize = 9;

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

/// The inline editor over the selected row's value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Editor {
    key: &'static str,
    buffer: String,
}

/// Rectangles from the last paint, for mouse hit-testing. Interior-mutable
/// for the same reason `SelectList::scroll` is: geometry is only known at
/// paint time and [`RenderOverlay::render`] takes `&self`.
#[derive(Debug, Clone, Copy, Default)]
struct Geometry {
    sections: Option<Rect>,
    rows: Rect,
    /// First visible row's index into the visible-row list.
    offset: usize,
}

/// The settings page overlay.
pub struct SettingsOverlay {
    /// The user's config file: read for the snapshot, written by edits.
    path: PathBuf,
    /// The path as the header shows it (`~` for the home directory).
    display_path: String,
    /// The resolved layer stack, or `None` when the file does not load.
    snapshot: Option<SettingsSnapshot>,
    /// Why the snapshot is `None`.
    load_error: Option<String>,
    /// Every setting the page offers: the schema catalogue plus the
    /// renderer's theme slots, in section order.
    specs: Vec<&'static SettingSpec>,
    /// The section shown while the query is empty.
    section: usize,
    /// The filter text.
    query: String,
    /// Selection index into the visible rows.
    selected: usize,
    /// Scroll offset into the visible rows; see [`SelectList`] for why it
    /// is a `Cell`.
    ///
    /// [`SelectList`]: super::SelectList
    scroll: Cell<usize>,
    /// Rows the list viewport held at the last paint.
    page: Cell<usize>,
    /// The inline editor, when one is open.
    editor: Option<Editor>,
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
    /// Last painted geometry for mouse hit-testing.
    geometry: Cell<Geometry>,
}

impl std::fmt::Debug for SettingsOverlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsOverlay")
            .field("path", &self.path)
            .field("loaded", &self.snapshot.is_some())
            .field("section", &SettingSection::ALL[self.section])
            .field("query", &self.query)
            .field("selected", &self.selected)
            .field("editor", &self.editor)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl SettingsOverlay {
    /// Open the page over the config file at `path`, styled with `theme`.
    ///
    /// Reads the file once here; every edit re-reads it, so the page always
    /// shows what is on disk rather than what it remembers writing.
    #[must_use]
    pub fn open(path: PathBuf, theme: &Theme) -> Self {
        let mut specs: Vec<&'static SettingSpec> = Vec::new();
        for section in SettingSection::ALL {
            specs.extend(CATALOG.iter().filter(|s| s.section == *section));
            specs.extend(SLOT_SPECS.iter().filter(|s| s.section == *section));
        }
        let display_path = shorten_home(&path);
        let mut page = Self {
            path,
            display_path,
            snapshot: None,
            load_error: None,
            specs,
            section: 0,
            query: String::new(),
            selected: 0,
            scroll: Cell::new(0),
            page: Cell::new(0),
            editor: None,
            status: None,
            undo: Vec::new(),
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
            geometry: Cell::new(Geometry::default()),
        };
        page.reload_snapshot();
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
    }

    // ---- data -----------------------------------------------------------

    /// Re-read the file and resolve the layer stack.
    fn reload_snapshot(&mut self) {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => {
                self.snapshot = None;
                self.load_error = Some(format!("cannot read {}: {err}", self.path.display()));
                return;
            }
        };
        match SettingsSnapshot::load(&text, &self.path) {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.load_error = None;
            }
            Err(err) => {
                self.snapshot = None;
                self.load_error = Some(err.to_string());
            }
        }
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
        let mut scored: Vec<(i32, usize)> = self
            .specs
            .iter()
            .enumerate()
            .filter_map(|(i, spec)| {
                let hay = format!("{} {}", spec.key, spec.summary).to_lowercase();
                fuzzy_score(&q, &hay).map(|score| (score, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, i)| self.specs[i]).collect()
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
        self.snapshot.as_ref()?.value_at(key).cloned()
    }

    /// The value the setting is *behaving* as: the merged value, or -- for
    /// a theme slot, which the schema leaves unset -- the renderer's
    /// default color. The schema's own defaults are already folded into the
    /// merged table, so for every other kind this is [`Self::value_at`].
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
        self.snapshot.as_ref()?.default_at(spec.key).cloned()
    }

    /// The layer above the defaults that set `key`, if any.
    fn origin_at(&self, key: &str) -> Option<LayerSource> {
        self.snapshot.as_ref()?.origin_at(key).cloned()
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
        self.scroll.set(0);
    }

    fn requery(&mut self) {
        self.selected = 0;
        self.scroll.set(0);
    }

    // ---- editing --------------------------------------------------------

    /// Enter, Space, or a click: the row's primary action.
    fn activate(&mut self, spec: &'static SettingSpec) -> OverlayCommand {
        match spec.kind {
            SettingKind::Bool => {
                let current = self.value_at(spec.key).and_then(|v| v.as_bool());
                self.apply(
                    spec,
                    Edit::Set(toml::Value::Boolean(!current.unwrap_or(false))),
                )
            }
            SettingKind::OptionalBool | SettingKind::Choice(_) => self.cycle(spec, 1),
            _ => {
                self.editor = Some(Editor {
                    key: spec.key,
                    buffer: self
                        .effective(spec)
                        .map(|v| render_value(Some(&v)))
                        .unwrap_or_default(),
                });
                self.status = None;
                OverlayCommand::Stay
            }
        }
    }

    /// Left/Right: step a choice, a tri-state, a bool, or an integer.
    fn cycle(&mut self, spec: &'static SettingSpec, direction: i64) -> OverlayCommand {
        match spec.kind {
            SettingKind::Bool => self.activate(spec),
            SettingKind::Choice(variants) => {
                let current = self.value_at(spec.key);
                let current = current.as_ref().and_then(toml::Value::as_str);
                let at = current
                    .and_then(|c| variants.iter().position(|v| *v == c))
                    .unwrap_or(0);
                let len = variants.len();
                let next = if direction >= 0 {
                    (at + 1) % len
                } else {
                    (at + len - 1) % len
                };
                self.apply(
                    spec,
                    Edit::Set(toml::Value::String(variants[next].to_owned())),
                )
            }
            SettingKind::OptionalBool => {
                // unset -> true -> false -> unset, and back the other way.
                let current = self.value_at(spec.key).and_then(|v| v.as_bool());
                let next = match (current, direction >= 0) {
                    (None, true) | (Some(false), false) => Edit::Set(toml::Value::Boolean(true)),
                    (Some(true), true) | (None, false) => Edit::Set(toml::Value::Boolean(false)),
                    (Some(false), true) | (Some(true), false) => Edit::Unset,
                };
                self.apply(spec, next)
            }
            SettingKind::Integer { min, max } | SettingKind::OptionalInteger { min, max } => {
                let Some(current) = self
                    .value_at(spec.key)
                    .or_else(|| self.default_at(spec))
                    .and_then(|v| v.as_integer())
                else {
                    return self.activate(spec);
                };
                let next = current.saturating_add(direction).clamp(min, max);
                if next == current {
                    return OverlayCommand::Stay;
                }
                self.apply(spec, Edit::Set(toml::Value::Integer(next)))
            }
            _ => OverlayCommand::Stay,
        }
    }

    /// Delete / `C-r`: remove the override so the shipped default shows
    /// through -- only when the override is in the user's own file.
    fn reset(&mut self, spec: &'static SettingSpec) -> OverlayCommand {
        match self.origin_at(spec.key) {
            None => {
                self.status = Some(Status::Note(format!(
                    "{} is already at its shipped default",
                    spec.key
                )));
                OverlayCommand::Stay
            }
            Some(LayerSource::Extended(layer)) => {
                self.status = Some(Status::Refused(format!(
                    "{} is set by the extends layer {}; edit that file to change it",
                    spec.key,
                    layer.display()
                )));
                OverlayCommand::Stay
            }
            Some(LayerSource::User(_) | LayerSource::Defaults) => self.apply(spec, Edit::Unset),
        }
    }

    /// `C-z`: put the last edited key back the way the file had it.
    fn undo_last(&mut self) -> OverlayCommand {
        let Some(undo) = self.undo.pop() else {
            self.status = Some(Status::Note("nothing to undo".to_owned()));
            return OverlayCommand::Stay;
        };
        match phux_config::settings::write_edit(&self.path, undo.key, undo.restore) {
            Ok(_) => {
                self.reload_snapshot();
                self.status = Some(Status::Saved(format!(
                    "undid the last change to {}",
                    undo.key
                )));
                OverlayCommand::ReloadConfig
            }
            Err(err) => {
                self.status = Some(Status::Refused(err.to_string()));
                OverlayCommand::Stay
            }
        }
    }

    /// Write one edit, re-read the file, and ask the driver to reload.
    fn apply(&mut self, spec: &'static SettingSpec, edit: Edit) -> OverlayCommand {
        match phux_config::settings::write_edit(&self.path, spec.key, edit) {
            Ok(outcome) => {
                self.undo.push(Undo {
                    key: spec.key,
                    restore: outcome.previous.map_or(Edit::Unset, Edit::Set),
                });
                self.reload_snapshot();
                self.status = Some(Status::Saved(outcome.line.map_or_else(
                    || self.removed_message(spec),
                    |line| format!("saved [{}] {line}", spec.table()),
                )));
                OverlayCommand::ReloadConfig
            }
            Err(err) => {
                self.status = Some(Status::Refused(err.to_string()));
                OverlayCommand::Stay
            }
        }
    }

    /// What an `Unset` left behind: the shipped default, or -- when an
    /// `extends` layer also sets the key -- that layer's value.
    fn removed_message(&self, spec: &SettingSpec) -> String {
        match self.origin_at(spec.key) {
            Some(LayerSource::Extended(layer)) => format!(
                "removed your override of {}; layer {} now sets it to {}",
                spec.key,
                layer.display(),
                render_value(self.effective(spec).as_ref())
            ),
            _ => format!(
                "removed {} (shipped default {} applies)",
                spec.key,
                render_value(self.default_at(spec).as_ref())
            ),
        }
    }

    /// Enter in the editor: parse the buffer for the row's kind and write.
    fn commit_editor(&mut self) -> OverlayCommand {
        let Some(editor) = self.editor.take() else {
            return OverlayCommand::Stay;
        };
        let Some(spec) = self.specs.iter().copied().find(|s| s.key == editor.key) else {
            return OverlayCommand::Stay;
        };
        match parse_input(spec.kind, editor.buffer.trim()) {
            Ok(edit) => self.apply(spec, edit),
            Err(message) => {
                self.status = Some(Status::Refused(format!("{}: {message}", spec.key)));
                // Keep the editor open with the text so the user can fix it.
                self.editor = Some(editor);
                OverlayCommand::Stay
            }
        }
    }

    fn handle_editor_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        match key.key {
            PhysicalKey::Escape => {
                self.editor = None;
                OverlayCommand::Stay
            }
            PhysicalKey::Enter | PhysicalKey::NumpadEnter => self.commit_editor(),
            PhysicalKey::Backspace => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.buffer.pop();
                }
                OverlayCommand::Stay
            }
            PhysicalKey::U if key.mods.contains(ModSet::CTRL) => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.buffer.clear();
                }
                OverlayCommand::Stay
            }
            _ => {
                if let Some(t) = &key.text
                    && !t.chars().any(char::is_control)
                    && let Some(editor) = self.editor.as_mut()
                {
                    editor.buffer.push_str(t);
                }
                OverlayCommand::Stay
            }
        }
    }

    // ---- rendering ------------------------------------------------------

    /// The modal rect: 80% of the viewport, at least 60x16, full-bleed on a
    /// starved axis.
    fn modal_area(outer: Rect, bp: ChromeBreakpoints) -> Rect {
        centered_panel(outer, 8, 60, 16, bp)
    }

    /// Whether the interior is wide enough for the section column.
    const fn use_two_columns(inner_width: u16, outer: Rect, bp: ChromeBreakpoints) -> bool {
        inner_width >= TWO_COLUMN_MIN_WIDTH && !bp.is_col_starved(outer.width)
    }

    /// Detail-panel rows for this viewport.
    const fn detail_rows(outer: Rect, bp: ChromeBreakpoints) -> u16 {
        if bp.is_row_starved(outer.height) {
            DETAIL_ROWS_COMPACT
        } else {
            DETAIL_ROWS
        }
    }

    /// Rows available to the list once the fixed chrome and the detail
    /// panel are taken out.
    const fn list_height(modal: Rect, detail: u16) -> usize {
        modal.height.saturating_sub(FIXED_ROWS + detail) as usize
    }

    /// The origin badge for `key`.
    fn badge(&self, key: &str) -> (String, Style) {
        match self.origin_at(key) {
            None => ("default".to_owned(), Style::default().fg(self.theme.dim)),
            Some(LayerSource::User(_)) => {
                ("you".to_owned(), Style::default().fg(self.theme.accent))
            }
            Some(LayerSource::Extended(path)) => {
                let name = path
                    .file_stem()
                    .map_or_else(|| "layer".to_owned(), |s| s.to_string_lossy().into_owned());
                (
                    clip_text(&name, BADGE_COLS - 1),
                    Style::default().fg(self.theme.section_header),
                )
            }
            Some(LayerSource::Defaults) => {
                ("default".to_owned(), Style::default().fg(self.theme.dim))
            }
        }
    }

    /// A row's value as spans: a swatch for a color, the editor field when
    /// this row is being edited, `unset` dimmed otherwise.
    fn value_spans(&self, spec: &SettingSpec, selected: bool) -> Vec<Span<'static>> {
        let base_fg = if selected {
            self.theme.selection_fg
        } else {
            self.theme.text
        };
        if let Some(editor) = &self.editor
            && editor.key == spec.key
        {
            return vec![
                Span::styled(editor.buffer.clone(), Style::default().fg(base_fg)),
                Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
            ];
        }
        let value = self.effective(spec);
        let Some(value) = value else {
            return vec![Span::styled(
                "unset".to_owned(),
                Style::default()
                    .fg(if selected { base_fg } else { self.theme.dim })
                    .add_modifier(Modifier::ITALIC),
            )];
        };
        let text = render_value(Some(&value));
        let mut spans = Vec::new();
        if spec.kind == SettingKind::Color
            && let Some(color) = value.as_str().and_then(|s| Color::from_str(s).ok())
        {
            spans.push(Span::styled(
                "\u{2588}\u{2588} ",
                Style::default().fg(color),
            ));
        }
        spans.push(Span::styled(text, Style::default().fg(base_fg)));
        spans
    }

    /// One list row laid out to exactly `width` cells: marker, key, value
    /// (right-aligned against the badge), badge.
    fn row_line(
        &self,
        spec: &SettingSpec,
        selected: bool,
        width: usize,
        full_key: bool,
    ) -> Line<'static> {
        let overridden = self.origin_at(spec.key).is_some();
        let marker = if overridden { "\u{2022} " } else { "  " };
        let key = if full_key { spec.key } else { spec.leaf() };
        let value_spans = self.value_spans(spec, selected);
        let value_w: usize = value_spans.iter().map(|s| display_width(&s.content)).sum();
        let (badge, badge_style) = self.badge(spec.key);
        let show_badge = width >= 40;
        let badge_w = if show_badge { BADGE_COLS } else { 0 };
        // The key takes what the value and badge leave, and at least a third
        // of the row; the value yields next; the badge is dropped whole on a
        // narrow row.
        let key_budget = width
            .saturating_sub(2 + badge_w + value_w + 1)
            .max(width / 3)
            .min(width.saturating_sub(2));
        let key_text = clip_text(key, key_budget);
        let value_budget = width.saturating_sub(2 + badge_w + display_width(&key_text) + 1);
        let value_fg = if selected {
            self.theme.selection_fg
        } else {
            self.theme.text
        };
        let value_spans: Vec<Span<'static>> = if value_w > value_budget {
            let joined: String = value_spans.iter().map(|s| s.content.as_ref()).collect();
            vec![Span::styled(
                clip_text(&joined, value_budget),
                Style::default().fg(value_fg),
            )]
        } else {
            value_spans
        };
        let value_w: usize = value_spans.iter().map(|s| display_width(&s.content)).sum();
        let pad = width.saturating_sub(2 + display_width(&key_text) + value_w + badge_w);
        let row_style = if selected {
            Style::default()
                .fg(self.theme.selection_fg)
                .bg(self.theme.selection_bg)
        } else {
            Style::default()
        };
        let key_style = row_style.fg(value_fg).add_modifier(if overridden {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });
        let marker_fg = if selected {
            self.theme.selection_fg
        } else {
            self.theme.accent
        };
        let mut spans = vec![
            Span::styled(marker.to_owned(), row_style.fg(marker_fg)),
            Span::styled(key_text, key_style),
            Span::styled(" ".repeat(pad), row_style),
        ];
        for span in value_spans {
            spans.push(Span::styled(span.content, span.style.patch(row_style)));
        }
        if show_badge {
            let badge_pad = BADGE_COLS.saturating_sub(display_width(&badge));
            spans.push(Span::styled(" ".repeat(badge_pad), row_style));
            spans.push(Span::styled(
                badge,
                if selected { row_style } else { badge_style },
            ));
        }
        Line::from(spans)
    }

    /// The section title row a single-column layout shows above its rows.
    fn header_line(&self, section: SettingSection) -> Line<'static> {
        Line::from(Span::styled(
            section.title().to_owned(),
            Style::default()
                .fg(self.theme.section_header)
                .add_modifier(Modifier::DIM),
        ))
    }

    /// The section column's row `index`, laid out to `SECTION_COL - 1`.
    fn section_line(&self, index: usize, width: usize) -> Line<'static> {
        let section = SettingSection::ALL[index];
        let active = self.query.is_empty() && index == self.section;
        let count = if self.query.is_empty() {
            self.overridden_in(section)
        } else {
            self.matches_in(section)
        };
        let marker = if active { "\u{25b8}" } else { " " };
        let count_text = if count > 0 {
            format!("{count}")
        } else {
            String::new()
        };
        let title_budget = width.saturating_sub(1 + count_text.len() + 1);
        let title = clip_text(section.title(), title_budget);
        let pad = width.saturating_sub(1 + display_width(&title) + count_text.len());
        let title_style = if active {
            Style::default()
                .fg(self.theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.theme.text)
        };
        Line::from(vec![
            Span::styled(marker.to_owned(), Style::default().fg(self.theme.accent)),
            Span::styled(title, title_style),
            Span::raw(" ".repeat(pad)),
            Span::styled(count_text, Style::default().fg(self.theme.dim)),
        ])
    }

    /// The detail panel: what the selected setting is, its default, its
    /// domain, when it applies, and the last outcome.
    fn detail_lines(
        &self,
        spec: Option<&SettingSpec>,
        width: usize,
        rows: u16,
    ) -> Vec<Line<'static>> {
        let dim = Style::default().fg(self.theme.dim);
        let text = Style::default().fg(self.theme.text);
        if let Some(err) = &self.load_error {
            return vec![
                Line::from(Span::styled(
                    clip_text(&format!("config does not load: {err}"), width),
                    Style::default().fg(self.theme.error),
                )),
                Line::from(Span::styled(
                    clip_text(
                        "Fix the file, then reopen this page (run: phux config check)",
                        width,
                    ),
                    dim,
                )),
            ];
        }
        let Some(spec) = spec else {
            return vec![Line::from(Span::styled(
                "(no setting matches)".to_owned(),
                dim,
            ))];
        };
        let mut lines = vec![
            // Line 1: key, kind.
            Line::from(vec![
                Span::styled(
                    spec.key.to_owned(),
                    Style::default()
                        .fg(self.theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("  {}", kind_label(spec.kind)), dim),
            ]),
            // Line 2: summary.
            Line::from(Span::styled(clip_text(spec.summary, width), text)),
        ];
        if rows <= DETAIL_ROWS_COMPACT {
            // The compact panel: the outcome line, when there is one, wins
            // over the summary.
            if let Some(status) = &self.status {
                lines.pop();
                lines.push(self.status_line(status, width));
            }
            return lines;
        }
        // Lines 3..: detail, wrapped, as many as fit above the two footer facts.
        let detail_budget = usize::from(rows).saturating_sub(4);
        for line in wrap_words(spec.detail, width)
            .into_iter()
            .take(detail_budget)
        {
            lines.push(Line::from(Span::styled(line, dim)));
        }
        lines.push(self.facts_line(spec));
        if let Some(line) = self.outcome_line(spec, width) {
            lines.push(line);
        }
        lines
    }

    /// `default X  ·  origin  ·  applies when`.
    fn facts_line(&self, spec: &SettingSpec) -> Line<'static> {
        let dim = Style::default().fg(self.theme.dim);
        let text = Style::default().fg(self.theme.text);
        let default = render_value(self.default_at(spec).as_ref());
        let origin = match self.origin_at(spec.key) {
            None | Some(LayerSource::Defaults) => "shipped default".to_owned(),
            Some(LayerSource::User(_)) => "set in your config".to_owned(),
            Some(LayerSource::Extended(path)) => format!("set by layer {}", path.display()),
        };
        let mut facts = vec![Span::styled("default ".to_owned(), dim)];
        if spec.kind == SettingKind::Color
            && let Ok(color) = Color::from_str(&default)
        {
            facts.push(Span::styled(
                "\u{2588} ".to_owned(),
                Style::default().fg(color),
            ));
        }
        facts.push(Span::styled(default, text));
        facts.push(Span::styled(format!("  \u{b7}  {origin}"), dim));
        facts.push(Span::styled(
            format!("  \u{b7}  applies {}", applies_label(spec.applies)),
            dim,
        ));
        Line::from(facts)
    }

    /// The last outcome, or a live color preview while a color is edited.
    fn outcome_line(&self, spec: &SettingSpec, width: usize) -> Option<Line<'static>> {
        let dim = Style::default().fg(self.theme.dim);
        if let Some(status) = &self.status {
            return Some(self.status_line(status, width));
        }
        let editor = self.editor.as_ref()?;
        if spec.kind != SettingKind::Color {
            return None;
        }
        Some(Color::from_str(editor.buffer.trim()).map_or_else(
            |_| {
                Line::from(Span::styled(
                    "not a color yet: use a name, #rrggbb, or an index 0-255".to_owned(),
                    dim,
                ))
            },
            |color| {
                Line::from(vec![
                    Span::styled("preview ".to_owned(), dim),
                    Span::styled(
                        "\u{2588}\u{2588}\u{2588}\u{2588}".to_owned(),
                        Style::default().fg(color),
                    ),
                ])
            },
        ))
    }

    fn status_line(&self, status: &Status, width: usize) -> Line<'static> {
        let (text, style) = match status {
            Status::Saved(t) => (t, Style::default().fg(self.theme.chord)),
            Status::Refused(t) => (t, Style::default().fg(self.theme.error)),
            Status::Note(t) => (t, Style::default().fg(self.theme.dim)),
        };
        Line::from(Span::styled(clip_text(text, width), style))
    }

    /// The header row: the filter prompt on the left, the file on the right.
    fn header_row(&self, width: usize) -> Line<'static> {
        let query_text = if self.query.is_empty() && self.editor.is_none() {
            "filter\u{2026}".to_owned()
        } else {
            self.query.clone()
        };
        let prompt_w = 2 + display_width(&query_text) + 1;
        let path = clip_text(&self.display_path, width.saturating_sub(prompt_w + 2));
        let pad = width.saturating_sub(prompt_w + display_width(&path));
        Line::from(vec![
            Span::styled("> ".to_owned(), Style::default().fg(self.theme.accent)),
            Span::styled(
                query_text,
                if self.query.is_empty() {
                    Style::default().fg(self.theme.dim)
                } else {
                    Style::default().fg(self.theme.text)
                },
            ),
            Span::styled(
                " ".to_owned(),
                if self.editor.is_none() {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                },
            ),
            Span::raw(" ".repeat(pad)),
            Span::styled(path, Style::default().fg(self.theme.dim)),
        ])
    }

    /// The `list_height` list rows: the section column (two-column layout)
    /// beside the optional section header and the windowed setting rows.
    fn list_rows(
        &self,
        specs: &[&'static SettingSpec],
        offset: usize,
        list_height: usize,
        header: usize,
        width: usize,
        two_column: bool,
    ) -> Vec<Line<'static>> {
        let section_w = usize::from(SECTION_COL);
        let row_w = if two_column {
            width.saturating_sub(section_w + display_width(SECTION_RULE))
        } else {
            width
        };
        let window = specs
            .get(offset..(offset + list_height.saturating_sub(header)).min(specs.len()))
            .unwrap_or(&[]);
        let mut lines = Vec::with_capacity(list_height);
        for i in 0..list_height {
            let mut spans: Vec<Span<'static>> = Vec::new();
            if two_column {
                if i < SettingSection::ALL.len() {
                    spans.extend(self.section_line(i, section_w).spans);
                } else {
                    spans.push(Span::raw(" ".repeat(section_w)));
                }
                spans.push(Span::styled(
                    SECTION_RULE.to_owned(),
                    Style::default().fg(self.theme.border),
                ));
            }
            if header == 1 && i == 0 {
                spans.extend(self.header_line(SettingSection::ALL[self.section]).spans);
            } else if let Some(spec) = window.get(i - header) {
                let selected = offset + i - header == self.selected;
                spans.extend(
                    self.row_line(spec, selected, row_w, !self.query.is_empty())
                        .spans,
                );
            } else if i == header && window.is_empty() {
                spans.push(Span::styled(
                    "(no matches)".to_owned(),
                    Style::default().fg(self.theme.dim),
                ));
            }
            lines.push(Line::from(spans));
        }
        lines
    }

    fn footer_hints(&self) -> Vec<&'static str> {
        if self.editor.is_some() {
            vec!["Enter apply", "Esc cancel", "C-u clear"]
        } else {
            vec![
                "Enter edit",
                "\u{2190}/\u{2192} step",
                "Del reset",
                "C-z undo",
                "Tab section",
                "type to filter",
                "Esc close",
            ]
        }
    }
}

/// A short kind description for the detail panel's first line.
fn kind_label(kind: SettingKind) -> String {
    match kind {
        SettingKind::Bool => "bool".to_owned(),
        SettingKind::Integer { min, max } => format!("integer {min}..{max}"),
        SettingKind::OptionalInteger { min, max } => format!("integer {min}..{max}, or unset"),
        SettingKind::Text => "text".to_owned(),
        SettingKind::OptionalText => "text, or unset".to_owned(),
        SettingKind::Choice(variants) => variants.join(" | "),
        SettingKind::OptionalBool => "unset | true | false".to_owned(),
        SettingKind::Argv => "command words, or unset".to_owned(),
        SettingKind::Chord => "chord".to_owned(),
        SettingKind::Color => "color: name, #rrggbb, or 0-255".to_owned(),
    }
}

/// When a change lands, in the page's words. `NextSpawn` is spelled as the
/// server behaves: `[defaults]` and `[voice]` are read once at server start.
const fn applies_label(applies: Applies) -> &'static str {
    match applies {
        Applies::LiveReload => "now (reloaded)",
        Applies::NextAttach => "next attach",
        Applies::NextSpawn => "next server start",
    }
}

/// Parse the editor's text into an edit for `kind`.
fn parse_input(kind: SettingKind, text: &str) -> Result<Edit, String> {
    let unset_word = text.is_empty() || text.eq_ignore_ascii_case("unset");
    match kind {
        SettingKind::Bool => parse_bool(text)
            .map(|b| Edit::Set(toml::Value::Boolean(b)))
            .ok_or_else(|| "expected true or false".to_owned()),
        SettingKind::OptionalBool => {
            if unset_word {
                return Ok(Edit::Unset);
            }
            parse_bool(text)
                .map(|b| Edit::Set(toml::Value::Boolean(b)))
                .ok_or_else(|| "expected true, false, or unset".to_owned())
        }
        SettingKind::Integer { min, max } => parse_int(text, min, max),
        SettingKind::OptionalInteger { min, max } => {
            if unset_word {
                Ok(Edit::Unset)
            } else {
                parse_int(text, min, max)
            }
        }
        SettingKind::Text | SettingKind::Chord => {
            if text.is_empty() {
                Err("cannot be empty; use Del to fall back to the shipped default".to_owned())
            } else {
                Ok(Edit::Set(toml::Value::String(text.to_owned())))
            }
        }
        SettingKind::OptionalText => {
            if unset_word {
                Ok(Edit::Unset)
            } else {
                Ok(Edit::Set(toml::Value::String(text.to_owned())))
            }
        }
        SettingKind::Choice(variants) => {
            let lowered = text.to_lowercase();
            variants
                .iter()
                .find(|v| **v == lowered)
                .map(|v| Edit::Set(toml::Value::String((*v).to_owned())))
                .ok_or_else(|| format!("expected one of {}", variants.join(", ")))
        }
        SettingKind::Argv => {
            if unset_word {
                return Ok(Edit::Unset);
            }
            let words = split_words(text)?;
            Ok(Edit::Set(toml::Value::Array(
                words.into_iter().map(toml::Value::String).collect(),
            )))
        }
        SettingKind::Color => {
            if text.is_empty() {
                return Err(
                    "cannot be empty; use Del to fall back to the shipped default".to_owned(),
                );
            }
            Color::from_str(text)
                .map(|_| Edit::Set(toml::Value::String(text.to_owned())))
                .map_err(|_| "not a color: use a name, #rrggbb, or an index 0-255".to_owned())
        }
    }
}

fn parse_bool(text: &str) -> Option<bool> {
    match text.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Some(true),
        "false" | "off" | "no" | "0" => Some(false),
        _ => None,
    }
}

fn parse_int(text: &str, min: i64, max: i64) -> Result<Edit, String> {
    let value: i64 = text
        .replace('_', "")
        .parse()
        .map_err(|_| format!("expected an integer between {min} and {max}"))?;
    if value < min || value > max {
        return Err(format!("must be between {min} and {max}"));
    }
    Ok(Edit::Set(toml::Value::Integer(value)))
}

/// Split a command line into words the way a POSIX shell would for the
/// common cases: whitespace separates, single quotes are literal, double
/// quotes allow `\"` and `\\`, a backslash escapes the next character.
fn split_words(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => current.push(ch),
                        None => return Err("unterminated single quote".to_owned()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(esc @ ('"' | '\\' | '$' | '`')) => current.push(esc),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => return Err("unterminated double quote".to_owned()),
                        },
                        Some(ch) => current.push(ch),
                        None => return Err("unterminated double quote".to_owned()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(ch) => current.push(ch),
                    None => return Err("trailing backslash".to_owned()),
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

/// Greedy word wrap to `width` cells; a word wider than the line is cut.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let word_w = display_width(word);
        let cur_w = display_width(&current);
        if cur_w == 0 {
            current.push_str(&clip_text(word, width));
        } else if cur_w + 1 + word_w <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(&clip_text(word, width));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// `~` for the home directory, so the header fits.
fn shorten_home(path: &Path) -> String {
    let shown = path.display().to_string();
    std::env::var_os("HOME")
        .map(|home| home.to_string_lossy().into_owned())
        .filter(|home| !home.is_empty())
        .and_then(|home| {
            shown
                .strip_prefix(home.as_str())
                .map(|rest| format!("~{rest}"))
        })
        .unwrap_or(shown)
}

impl SettingsOverlay {
    /// The `C-` chords: `C-n`/`C-p` move, `C-r` resets, `C-z` undoes, `C-u`
    /// clears the query. `None` for a chord this page does not bind.
    fn handle_ctrl_key(&mut self, key: PhysicalKey, len: usize) -> Option<OverlayCommand> {
        Some(match key {
            PhysicalKey::N => {
                self.select_down(len);
                OverlayCommand::Stay
            }
            PhysicalKey::P => {
                self.select_up();
                OverlayCommand::Stay
            }
            PhysicalKey::R => self
                .selected_spec()
                .map_or(OverlayCommand::Stay, |spec| self.reset(spec)),
            PhysicalKey::Z => self.undo_last(),
            PhysicalKey::U => {
                self.query.clear();
                self.requery();
                OverlayCommand::Stay
            }
            _ => return None,
        })
    }

    /// Selection movement: arrows, `j`/`k` while the query is empty, page
    /// and home/end keys. `true` when `key` was one of them.
    fn navigate(&mut self, key: PhysicalKey, len: usize) -> bool {
        match key {
            PhysicalKey::ArrowDown => self.select_down(len),
            PhysicalKey::ArrowUp => self.select_up(),
            PhysicalKey::J if self.query.is_empty() => self.select_down(len),
            PhysicalKey::K if self.query.is_empty() => self.select_up(),
            PhysicalKey::PageDown => {
                for _ in 0..self.page_rows() {
                    self.select_down(len);
                }
            }
            PhysicalKey::PageUp => {
                for _ in 0..self.page_rows() {
                    self.select_up();
                }
            }
            PhysicalKey::Home => self.selected = 0,
            PhysicalKey::End => self.selected = len.saturating_sub(1),
            _ => return false,
        }
        true
    }
}

/// A viewport-cell coordinate from the pointer's f64 position.
fn cell_coord(value: f64) -> u16 {
    let clamped = value.max(0.0).min(f64::from(u16::MAX));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=u16::MAX on the line above"
    )]
    {
        clamped as u16
    }
}

impl RenderOverlay for SettingsOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let modal = Self::modal_area(area, self.breakpoints);
        let inner_width = modal.width.saturating_sub(2);
        let width = usize::from(inner_width);
        let two_column = Self::use_two_columns(inner_width, area, self.breakpoints);
        let detail = Self::detail_rows(area, self.breakpoints);
        let list_height = Self::list_height(modal, detail);
        // A single-column layout spends its first list row on the section
        // title while browsing; a filtered list shows full keys instead.
        let header = usize::from(!two_column && self.query.is_empty());
        let spec_rows = list_height.saturating_sub(header);
        let specs = self.visible_specs();

        let mut lines: Vec<Line<'static>> = vec![self.header_row(width), Line::from("")];
        let offset = scroll_into_view(self.scroll.get(), self.selected, specs.len(), spec_rows);
        self.page.set(spec_rows);
        self.scroll.set(offset);
        lines.extend(self.list_rows(&specs, offset, list_height, header, width, two_column));
        lines.push(Line::from(Span::styled(
            "\u{2500}".repeat(width),
            Style::default().fg(self.theme.border),
        )));
        let mut detail_lines = self.detail_lines(specs.get(self.selected).copied(), width, detail);
        detail_lines.truncate(usize::from(detail));
        while detail_lines.len() < usize::from(detail) {
            detail_lines.push(Line::from(""));
        }
        lines.extend(detail_lines);

        Modal::new(&self.theme, "Settings", lines)
            .footer_hints(self.footer_hints())
            .render_into(modal, buf);

        // Scrollbar over the setting rows only.
        let list_top = modal.y.saturating_add(3);
        let rows_top = list_top.saturating_add(u16::try_from(header).unwrap_or(0));
        let rows_height = u16::try_from(spec_rows).unwrap_or(u16::MAX);
        paint_scrollbar(
            buf,
            Rect::new(
                modal.x + modal.width.saturating_sub(1),
                rows_top,
                1,
                rows_height,
            ),
            &self.theme,
            specs.len(),
            offset,
        );

        // Geometry for the mouse.
        let rule_w = u16::try_from(display_width(SECTION_RULE)).unwrap_or(3);
        let rows_x = if two_column {
            modal.x + 1 + SECTION_COL + rule_w
        } else {
            modal.x + 1
        };
        let rows_right = modal.x + modal.width.saturating_sub(1);
        self.geometry.set(Geometry {
            sections: two_column.then(|| {
                Rect::new(
                    modal.x + 1,
                    list_top,
                    SECTION_COL,
                    u16::try_from(SettingSection::ALL.len().min(list_height)).unwrap_or(0),
                )
            }),
            rows: Rect::new(
                rows_x,
                rows_top,
                rows_right.saturating_sub(rows_x),
                rows_height,
            ),
            offset,
        });
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(Self::modal_area(area, self.breakpoints))
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = *theme;
    }

    fn handle_paste(&mut self, text: &str) {
        if let Some(editor) = self.editor.as_mut() {
            editor.buffer.push_str(text);
        } else {
            self.query.push_str(text);
            self.requery();
        }
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        if mouse.action != MouseAction::Press {
            return OverlayCommand::Stay;
        }
        let len = self.visible_specs().len();
        self.clamp_selection(len);
        match mouse.button {
            MouseButton::Four => {
                for _ in 0..WHEEL_SCROLL_ROWS {
                    self.select_up();
                }
            }
            MouseButton::Five => {
                for _ in 0..WHEEL_SCROLL_ROWS {
                    self.select_down(len);
                }
            }
            MouseButton::Left => {
                let geometry = self.geometry.get();
                let (col, row) = (cell_coord(mouse.x), cell_coord(mouse.y));
                if let Some(sections) = geometry.sections
                    && sections.contains((col, row).into())
                {
                    let index = usize::from(row - sections.y);
                    if index < SettingSection::ALL.len() {
                        self.select_section(index);
                    }
                } else if geometry.rows.contains((col, row).into()) {
                    let index = geometry.offset + usize::from(row - geometry.rows.y);
                    if index < len {
                        self.selected = index;
                    }
                }
            }
            _ => {}
        }
        OverlayCommand::Stay
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        if self.editor.is_some() {
            return self.handle_editor_key(key);
        }
        let len = self.visible_specs().len();
        self.clamp_selection(len);
        if key.mods.contains(ModSet::CTRL)
            && let Some(command) = self.handle_ctrl_key(key.key, len)
        {
            return command;
        }
        match key.key {
            PhysicalKey::Escape => {
                if self.query.is_empty() {
                    OverlayCommand::Dismiss
                } else {
                    self.query.clear();
                    self.requery();
                    OverlayCommand::Stay
                }
            }
            PhysicalKey::Enter | PhysicalKey::NumpadEnter => self
                .selected_spec()
                .map_or(OverlayCommand::Stay, |spec| self.activate(spec)),
            // Space acts on the row only while the query is empty; inside a
            // query it is filter text like any other character.
            PhysicalKey::Space if self.query.is_empty() => self
                .selected_spec()
                .map_or(OverlayCommand::Stay, |spec| self.activate(spec)),
            PhysicalKey::Delete => self
                .selected_spec()
                .map_or(OverlayCommand::Stay, |spec| self.reset(spec)),
            PhysicalKey::Tab => {
                let count = SettingSection::ALL.len();
                let next = if key.mods.contains(ModSet::SHIFT) {
                    (self.section + count - 1) % count
                } else {
                    (self.section + 1) % count
                };
                self.select_section(next);
                OverlayCommand::Stay
            }
            // Only the arrow keys step a value: a letter is always filter
            // text, so the first keystroke of a query can never write the
            // file.
            PhysicalKey::ArrowRight => self
                .selected_spec()
                .map_or(OverlayCommand::Stay, |spec| self.cycle(spec, 1)),
            PhysicalKey::ArrowLeft => self
                .selected_spec()
                .map_or(OverlayCommand::Stay, |spec| self.cycle(spec, -1)),
            PhysicalKey::Backspace => {
                if self.query.pop().is_some() {
                    self.requery();
                }
                OverlayCommand::Stay
            }
            navigation if self.navigate(navigation, len) => OverlayCommand::Stay,
            _ => {
                if let Some(t) = &key.text
                    && !t.chars().any(char::is_control)
                {
                    self.query.push_str(t);
                    self.status = None;
                    self.requery();
                }
                OverlayCommand::Stay
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;

    fn press(key: PhysicalKey, text: Option<&str>) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: text.map(ToOwned::to_owned),
            unshifted_codepoint: None,
        }
    }

    fn ctrl(key: PhysicalKey) -> KeyEvent {
        let mut ev = press(key, None);
        ev.mods = ModSet::CTRL;
        ev
    }

    fn type_text(page: &mut SettingsOverlay, text: &str) {
        for ch in text.chars() {
            page.handle_key(&press(PhysicalKey::A, Some(&ch.to_string())));
        }
    }

    /// A page over a fresh temp config holding `toml`.
    fn page_over(toml: &str) -> (tempfile::TempDir, SettingsOverlay) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, toml).expect("write config");
        let page = SettingsOverlay::open(path, &Theme::default())
            .with_display_path("~/.config/phux/config.toml");
        (dir, page)
    }

    fn file(dir: &tempfile::TempDir) -> String {
        std::fs::read_to_string(dir.path().join("config.toml")).expect("read config")
    }

    fn render_text(page: &SettingsOverlay, cols: u16, rows: u16) -> String {
        let area = Rect::new(0, 0, cols, rows);
        let mut buf = Buffer::empty(area);
        page.render(area, &mut buf);
        let mut out = String::new();
        for y in 0..area.height {
            let mut row = String::new();
            for x in 0..area.width {
                row.push_str(buf[(x, y)].symbol());
            }
            out.push_str(row.trim_end());
            out.push('\n');
        }
        out
    }

    #[test]
    fn opens_on_the_first_section_with_the_schema_and_theme_rows() {
        let (_dir, page) = page_over("");
        assert!(page.snapshot.is_some());
        assert_eq!(page.section, 0);
        let rows = page.visible_specs();
        assert!(rows.iter().all(|s| s.section == SettingSection::Defaults));
        assert!(
            page.specs.iter().any(|s| s.key == "theme.accent"),
            "theme slots are rows"
        );
        assert!(page.specs.iter().any(|s| s.key == "sidebar.width"));
    }

    #[test]
    fn toggling_a_bool_writes_the_file_and_requests_a_reload() {
        let (dir, mut page) = page_over("# my config\n");
        page.focus("sidebar.enabled");
        assert_eq!(
            page.handle_key(&press(PhysicalKey::Enter, None)),
            OverlayCommand::ReloadConfig
        );
        let text = file(&dir);
        assert!(
            text.starts_with("# my config\n"),
            "comments survive: {text}"
        );
        assert!(text.contains("[sidebar]\nenabled = false"), "{text}");
        assert!(
            matches!(page.status, Some(Status::Saved(_))),
            "{:?}",
            page.status
        );
        assert!(matches!(
            page.origin_at("sidebar.enabled"),
            Some(LayerSource::User(_))
        ));
        // And back.
        page.handle_key(&press(PhysicalKey::Space, None));
        assert!(file(&dir).contains("enabled = true"));
    }

    #[test]
    fn cycling_a_choice_steps_through_its_variants_and_wraps() {
        let (dir, mut page) = page_over("");
        page.focus("sidebar.position");
        assert_eq!(
            page.handle_key(&press(PhysicalKey::ArrowRight, None)),
            OverlayCommand::ReloadConfig
        );
        assert!(file(&dir).contains("position = \"right\""));
        page.handle_key(&press(PhysicalKey::ArrowRight, None));
        assert!(
            file(&dir).contains("position = \"left\""),
            "wraps: {}",
            file(&dir)
        );
        page.handle_key(&press(PhysicalKey::ArrowLeft, None));
        assert!(file(&dir).contains("position = \"right\""));
    }

    #[test]
    fn arrows_step_an_integer_within_its_bounds() {
        let (dir, mut page) = page_over("[chrome]\ncompact-cols = 0\n");
        page.focus("chrome.compact-cols");
        assert_eq!(
            page.handle_key(&press(PhysicalKey::ArrowLeft, None)),
            OverlayCommand::Stay,
            "already at the lower bound"
        );
        page.handle_key(&press(PhysicalKey::ArrowRight, None));
        assert!(file(&dir).contains("compact-cols = 1"), "{}", file(&dir));
    }

    #[test]
    fn editor_commits_a_valid_integer_and_refuses_an_invalid_one() {
        let (dir, mut page) = page_over("");
        page.focus("sidebar.width");
        page.handle_key(&press(PhysicalKey::Enter, None));
        assert_eq!(
            page.editor.as_ref().map(|e| e.buffer.as_str()),
            Some("0"),
            "the editor opens on the effective value (0 = automatic width)"
        );
        page.handle_key(&ctrl(PhysicalKey::U));
        type_text(&mut page, "32");
        assert_eq!(
            page.handle_key(&press(PhysicalKey::Enter, None)),
            OverlayCommand::ReloadConfig
        );
        assert!(page.editor.is_none());
        assert!(file(&dir).contains("width = 32"), "{}", file(&dir));

        page.handle_key(&press(PhysicalKey::Enter, None));
        page.handle_key(&ctrl(PhysicalKey::U));
        type_text(&mut page, "wide");
        assert_eq!(
            page.handle_key(&press(PhysicalKey::Enter, None)),
            OverlayCommand::Stay
        );
        assert!(
            matches!(page.status, Some(Status::Refused(_))),
            "{:?}",
            page.status
        );
        assert!(
            page.editor.is_some(),
            "the editor stays open to fix the text"
        );
        assert!(file(&dir).contains("width = 32"), "file untouched");
        page.handle_key(&press(PhysicalKey::Escape, None));
        assert!(page.editor.is_none());
    }

    #[test]
    fn a_value_the_checker_rejects_is_refused_with_its_reason() {
        let (dir, mut page) = page_over("");
        page.focus("keybindings.prefix");
        page.handle_key(&press(PhysicalKey::Enter, None));
        page.handle_key(&ctrl(PhysicalKey::U));
        type_text(&mut page, "not a chord");
        assert_eq!(
            page.handle_key(&press(PhysicalKey::Enter, None)),
            OverlayCommand::Stay
        );
        match &page.status {
            Some(Status::Refused(msg)) => assert!(msg.contains("keybindings.prefix"), "{msg}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(!file(&dir).contains("prefix"), "nothing written");
    }

    #[test]
    fn reset_removes_the_override_and_undo_restores_it() {
        let (dir, mut page) = page_over("[sidebar]\nwidth = 40 # wide\n");
        page.focus("sidebar.width");
        assert!(page.origin_at("sidebar.width").is_some());
        assert_eq!(
            page.handle_key(&press(PhysicalKey::Delete, None)),
            OverlayCommand::ReloadConfig
        );
        assert!(!file(&dir).contains("width"), "{}", file(&dir));
        assert!(page.origin_at("sidebar.width").is_none());
        assert_eq!(
            page.value_at("sidebar.width"),
            Some(toml::Value::Integer(0)),
            "the shipped default (0 = automatic width) shows through"
        );

        assert_eq!(
            page.handle_key(&ctrl(PhysicalKey::Z)),
            OverlayCommand::ReloadConfig
        );
        assert!(
            file(&dir).contains("width = 40"),
            "undo restores: {}",
            file(&dir)
        );

        // A second undo has nothing left.
        assert_eq!(page.handle_key(&ctrl(PhysicalKey::Z)), OverlayCommand::Stay);
        assert!(matches!(page.status, Some(Status::Note(_))));
    }

    #[test]
    fn reset_of_a_default_is_a_note_not_a_write() {
        let (dir, mut page) = page_over("");
        page.focus("sidebar.width");
        assert_eq!(page.handle_key(&ctrl(PhysicalKey::R)), OverlayCommand::Stay);
        assert!(matches!(page.status, Some(Status::Note(_))));
        assert_eq!(file(&dir), "");
    }

    #[test]
    fn reset_is_refused_for_a_key_set_by_an_extends_layer() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("team.toml"), "[sidebar]\nwidth = 50\n").expect("layer");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "extends = [\"team.toml\"]\n").expect("config");
        let mut page = SettingsOverlay::open(path, &Theme::default());
        page.focus("sidebar.width");
        assert_eq!(
            page.value_at("sidebar.width"),
            Some(toml::Value::Integer(50))
        );
        assert!(matches!(
            page.origin_at("sidebar.width"),
            Some(LayerSource::Extended(_))
        ));
        assert_eq!(
            page.handle_key(&press(PhysicalKey::Delete, None)),
            OverlayCommand::Stay
        );
        match &page.status {
            Some(Status::Refused(msg)) => assert!(msg.contains("team.toml"), "{msg}"),
            other => panic!("expected a refusal naming the layer, got {other:?}"),
        }
        let (badge, _) = page.badge("sidebar.width");
        assert_eq!(badge, "team");
        // Overriding it from the page is still allowed.
        page.handle_key(&press(PhysicalKey::ArrowRight, None));
        assert!(file(&dir).contains("width = 51"), "{}", file(&dir));
    }

    #[test]
    fn theme_slots_edit_colors_with_validation_and_a_preview() {
        let (dir, mut page) = page_over("");
        page.focus("theme.accent");
        assert_eq!(
            page.default_at(page.selected_spec().unwrap()),
            Some(toml::Value::String(crate::render::theme::color_to_string(
                Theme::default().accent
            )))
        );
        page.handle_key(&press(PhysicalKey::Enter, None));
        page.handle_key(&ctrl(PhysicalKey::U));
        type_text(&mut page, "#ff0000");
        assert_eq!(
            page.handle_key(&press(PhysicalKey::Enter, None)),
            OverlayCommand::ReloadConfig
        );
        assert!(
            file(&dir).contains("[theme]\naccent = \"#ff0000\""),
            "{}",
            file(&dir)
        );

        page.handle_key(&press(PhysicalKey::Enter, None));
        page.handle_key(&ctrl(PhysicalKey::U));
        type_text(&mut page, "notacolor");
        assert_eq!(
            page.handle_key(&press(PhysicalKey::Enter, None)),
            OverlayCommand::Stay
        );
        assert!(matches!(page.status, Some(Status::Refused(_))));
        assert!(
            file(&dir).contains("#ff0000"),
            "file untouched by the refusal"
        );
    }

    #[test]
    fn argv_and_optional_kinds_round_trip_unset() {
        let (dir, mut page) = page_over("");
        page.focus("voice.transcriber");
        page.handle_key(&press(PhysicalKey::Enter, None));
        type_text(&mut page, "curl -sf 'a b' \"c\"");
        page.handle_key(&press(PhysicalKey::Enter, None));
        assert!(
            file(&dir).contains("transcriber = [\"curl\", \"-sf\", \"a b\", \"c\"]"),
            "{}",
            file(&dir)
        );
        page.handle_key(&press(PhysicalKey::Enter, None));
        page.handle_key(&ctrl(PhysicalKey::U));
        type_text(&mut page, "unset");
        page.handle_key(&press(PhysicalKey::Enter, None));
        assert!(!file(&dir).contains("transcriber"), "{}", file(&dir));

        page.focus("experimental.predictive-echo");
        page.handle_key(&press(PhysicalKey::Space, None));
        assert!(file(&dir).contains("predictive-echo = true"));
        page.handle_key(&press(PhysicalKey::Space, None));
        assert!(file(&dir).contains("predictive-echo = false"));
        page.handle_key(&press(PhysicalKey::Space, None));
        assert!(!file(&dir).contains("predictive-echo"), "third step unsets");
    }

    #[test]
    fn letters_never_write_the_file() {
        // The regression the review caught: `h`/`l` used to step the value.
        let (dir, mut page) = page_over("");
        page.focus("sidebar.enabled");
        for ch in ['h', 'l', 'j', 'k', ' '] {
            let key = match ch {
                'h' => PhysicalKey::H,
                'l' => PhysicalKey::L,
                'j' => PhysicalKey::J,
                'k' => PhysicalKey::K,
                _ => PhysicalKey::Space,
            };
            page.query = "x".to_owned();
            assert_eq!(
                page.handle_key(&press(key, Some(&ch.to_string()))),
                OverlayCommand::Stay
            );
            assert_eq!(file(&dir), "", "`{ch}` inside a query must not write");
        }
        page.query.clear();
        page.handle_key(&press(PhysicalKey::L, Some("l")));
        page.handle_key(&press(PhysicalKey::H, Some("h")));
        assert_eq!(file(&dir), "", "letters filter even with an empty query");
        assert_eq!(page.query, "lh");
    }

    #[test]
    fn a_reset_that_uncovers_a_layer_says_so() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("team.toml"), "[sidebar]\nwidth = 50\n").expect("layer");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "extends = [\"team.toml\"]\n[sidebar]\nwidth = 40\n")
            .expect("config");
        let mut page = SettingsOverlay::open(path, &Theme::default());
        page.focus("sidebar.width");
        assert_eq!(
            page.handle_key(&press(PhysicalKey::Delete, None)),
            OverlayCommand::ReloadConfig
        );
        match &page.status {
            Some(Status::Saved(msg)) => {
                assert!(msg.contains("team.toml") && msg.contains("50"), "{msg}");
            }
            other => panic!("expected a saved note naming the layer, got {other:?}"),
        }
    }

    #[test]
    fn filtering_flattens_sections_and_escape_clears_before_closing() {
        let (_dir, mut page) = page_over("");
        type_text(&mut page, "which");
        let keys: Vec<&str> = page.visible_specs().iter().map(|s| s.key).collect();
        assert!(keys.contains(&"keybindings.which-key"), "{keys:?}");
        assert!(keys.contains(&"keybindings.which-key-delay-ms"), "{keys:?}");
        assert!(page.matches_in(SettingSection::Keybindings) >= 2);
        assert_eq!(page.matches_in(SettingSection::Voice), 0);
        assert_eq!(
            page.handle_key(&press(PhysicalKey::Escape, None)),
            OverlayCommand::Stay
        );
        assert!(page.query.is_empty());
        assert_eq!(
            page.handle_key(&press(PhysicalKey::Escape, None)),
            OverlayCommand::Dismiss
        );
    }

    #[test]
    fn tab_steps_sections_and_shift_tab_steps_back() {
        let (_dir, mut page) = page_over("");
        page.handle_key(&press(PhysicalKey::Tab, None));
        assert_eq!(
            SettingSection::ALL[page.section],
            SettingSection::Keybindings
        );
        let mut back = press(PhysicalKey::Tab, None);
        back.mods = ModSet::SHIFT;
        page.handle_key(&back);
        assert_eq!(SettingSection::ALL[page.section], SettingSection::Defaults);
        page.handle_key(&back);
        assert_eq!(
            SettingSection::ALL[page.section],
            SettingSection::Voice,
            "wraps"
        );
    }

    #[test]
    fn a_config_that_does_not_load_shows_the_error_instead_of_rows() {
        let (_dir, mut page) = page_over("this is [not toml");
        assert!(page.snapshot.is_none());
        assert!(page.load_error.is_some());
        assert_eq!(
            page.handle_key(&press(PhysicalKey::Enter, None)),
            OverlayCommand::Stay
        );
        let text = render_text(&page, 90, 24);
        assert!(text.contains("config does not load"), "{text}");
    }

    #[test]
    fn overridden_rows_count_in_the_section_column() {
        let (_dir, page) = page_over("[sidebar]\nwidth = 40\nenabled = false\n");
        assert_eq!(page.overridden_in(SettingSection::Sidebar), 2);
        assert_eq!(page.overridden_in(SettingSection::Chrome), 0);
    }

    #[test]
    fn mouse_click_selects_a_row_and_a_section() {
        let (_dir, mut page) = page_over("");
        // Paint once so the geometry is known.
        let _ = render_text(&page, 100, 30);
        let geometry = page.geometry.get();
        let sections = geometry.sections.expect("two-column layout at 100 cols");
        let click = |x: u16, y: u16| MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x: f64::from(x),
            y: f64::from(y),
        };
        page.handle_mouse(&click(sections.x + 1, sections.y + 3));
        assert_eq!(SettingSection::ALL[page.section], SettingSection::Sidebar);
        let _ = render_text(&page, 100, 30);
        let rows = page.geometry.get().rows;
        page.handle_mouse(&click(rows.x + 2, rows.y + 2));
        assert_eq!(page.selected, 2);
        assert_eq!(
            page.selected_spec().map(|s| s.key),
            Some("sidebar.position")
        );
    }

    #[test]
    fn render_roomy_layout_is_stable() {
        let (_dir, mut page) = page_over("[sidebar]\nwidth = 40\n");
        page.focus("sidebar.width");
        insta::assert_snapshot!(render_text(&page, 100, 30));
    }

    #[test]
    fn render_compact_layout_is_stable() {
        let (_dir, mut page) = page_over("");
        page.focus("keybindings.which-key");
        // 60x16 is starved on both axes at the shipped breakpoints: full
        // bleed, single column, the short detail panel. The selection is the
        // same row the two-column layout would show, header or not.
        let text = render_text(&page, 60, 16);
        assert!(text.contains("keybindings.which-key  bool"), "{text}");
        insta::assert_snapshot!(text);
    }

    #[test]
    fn render_filtered_layout_is_stable() {
        let (_dir, mut page) = page_over("[theme]\naccent = \"#ff0000\"\n");
        type_text(&mut page, "accent");
        insta::assert_snapshot!(render_text(&page, 100, 24));
    }

    #[test]
    fn split_words_handles_quotes_and_escapes() {
        assert_eq!(
            split_words("curl -sf 'a b' \"c \\\" d\" e\\ f").unwrap(),
            vec!["curl", "-sf", "a b", "c \" d", "e f"]
        );
        assert_eq!(split_words("   ").unwrap(), Vec::<String>::new());
        assert!(split_words("'open").is_err());
        assert!(split_words("trailing\\").is_err());
    }

    #[test]
    fn wrap_words_fills_lines_and_cuts_long_words() {
        assert_eq!(
            wrap_words("one two three four", 9),
            vec!["one two", "three", "four"]
        );
        assert_eq!(wrap_words("abcdefghij", 4), vec!["abc\u{2026}"]);
        assert!(wrap_words("x", 0).is_empty());
    }

    #[test]
    fn parse_input_covers_every_kind() {
        assert_eq!(
            parse_input(SettingKind::Bool, "on"),
            Ok(Edit::Set(toml::Value::Boolean(true)))
        );
        assert!(parse_input(SettingKind::Bool, "maybe").is_err());
        assert_eq!(parse_input(SettingKind::OptionalBool, ""), Ok(Edit::Unset));
        assert_eq!(
            parse_input(SettingKind::Integer { min: 0, max: 10 }, "7"),
            Ok(Edit::Set(toml::Value::Integer(7)))
        );
        assert!(parse_input(SettingKind::Integer { min: 0, max: 10 }, "11").is_err());
        assert_eq!(
            parse_input(SettingKind::OptionalInteger { min: 1, max: 9 }, "unset"),
            Ok(Edit::Unset)
        );
        assert_eq!(
            parse_input(SettingKind::Choice(&["left", "right"]), "RIGHT"),
            Ok(Edit::Set(toml::Value::String("right".to_owned())))
        );
        assert!(parse_input(SettingKind::Choice(&["left", "right"]), "up").is_err());
        assert!(parse_input(SettingKind::Text, "").is_err());
        assert_eq!(parse_input(SettingKind::OptionalText, ""), Ok(Edit::Unset));
        assert!(parse_input(SettingKind::Color, "#12345").is_err());
        assert!(parse_input(SettingKind::Color, "cyan").is_ok());
    }
}
