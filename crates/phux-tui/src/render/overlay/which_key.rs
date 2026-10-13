//! Which-key popup.
//!
//! After the prefix and `which-key-delay-ms` of hesitation, a floating legend
//! of every prefix-table continuation from the live [`KeybindingsCfg`],
//! grouped by what it acts on (pane, window, session, phux itself). Keys bound
//! to the same action share one row, numeric window jumps collapse to `0-9`,
//! and a directional family (`h j k l` focus) collapses to one row.
//!
//! It is transparent to input ([`RenderOverlay::is_input_passthrough`]): the
//! next key dismisses it and runs as if it had never appeared (Esc also
//! cancels the prefix), so it can never eat or delay a chord.

use phux_config::{Action, KeybindingsCfg};
use phux_protocol::input::key::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::widgets::{Modal, centered_panel, is_compact};
use super::{OverlayCommand, RenderOverlay};
use crate::render::{ChromeBreakpoints, Theme};

/// The sections the legend groups bindings under, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Group {
    Pane,
    Window,
    Session,
    Phux,
}

impl Group {
    const fn header(self) -> &'static str {
        match self {
            Self::Pane => "Pane",
            Self::Window => "Window",
            Self::Session => "Session",
            Self::Phux => "phux",
        }
    }

    /// The section an action belongs to, from its name: anything that is
    /// not about a pane, a window, or a session is phux's own chrome.
    fn of(action: &str) -> Self {
        const PANE: &[&str] = &[
            "copy-mode",
            "find-path",
            "insert-path",
            "take-input",
            "give-input",
            "signal-terminal",
            "toggle-zoom",
            "focus-direction",
        ];
        if action.ends_with("-pane") || PANE.contains(&action) {
            Self::Pane
        } else if action.contains("window") || action == "go-to-directory" {
            Self::Window
        } else if action.contains("session") || action == "detach" {
            Self::Session
        } else {
            Self::Phux
        }
    }
}

/// Reading order inside a group; unlisted actions follow, by label.
const ACTION_ORDER: &[&str] = &[
    "split-pane",
    "kill-pane",
    "focus-direction",
    "resize-pane",
    "next-pane",
    "previous-pane",
    "last-pane",
    "toggle-zoom",
    "move-pane",
    "copy-mode",
    "find-path",
    "take-input",
    "give-input",
    "signal-terminal",
    "new-window",
    "kill-window",
    "select-window",
    "next-window",
    "previous-window",
    "window-picker",
    "rename-window",
    "move-window",
    "go-to-directory",
    "new-session",
    "session-picker",
    "next-session",
    "previous-session",
    "last-session",
    "rename-session",
    "detach",
    "command-palette",
    "show-help",
    "agent-fleet",
    "next-attention",
    "return-from-attention",
    "toggle-sidebar",
    "settings",
    "reload-config",
    "report-bug",
];

/// Actions whose bindings differ only by `direction` and read best as one
/// row (`h j k l  focus`), with that row's label.
const DIRECTIONAL: &[(&str, &str)] = &[("focus-direction", "focus"), ("resize-pane", "resize")];

/// Cardinal directions in the order a directional row lists its keys.
const DIRECTIONS: [&str; 4] = ["left", "down", "up", "right"];

/// One legend row: the keys that reach it and what they do.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    group: Group,
    /// Sort key inside the group (see [`ACTION_ORDER`]).
    rank: usize,
    keys: Vec<String>,
    label: String,
}

impl Row {
    fn key_text(&self) -> String {
        self.keys.join(" ")
    }
}

/// Which-key popup: prefix-table continuations as grouped `key  action` rows.
#[derive(Debug)]
pub struct WhichKeyOverlay {
    /// Pretty-printed prefix chord as authored in config (e.g. `"C-a"`),
    /// shown in the title so the user sees which pending prefix this is.
    prefix: String,
    /// Rows in display order: by group, then [`ACTION_ORDER`], then label.
    rows: Vec<Row>,
    /// Color slots snapshotted from the active [`Theme`] at construction.
    theme: Theme,
    /// `[chrome]` thresholds, stamped by `OverlayState::push`.
    breakpoints: ChromeBreakpoints,
}

impl WhichKeyOverlay {
    /// Build the popup from the live config snapshot, styled with `theme`.
    #[must_use]
    pub fn from_config(cfg: &KeybindingsCfg, theme: &Theme) -> Self {
        let mut rows: Vec<Row> = Vec::new();
        let mut window_jump_keys: Vec<String> = Vec::new();
        // (family, direction, key) for every directional binding.
        let mut directional: Vec<(usize, usize, &str)> = Vec::new();
        for (key, action) in &cfg.prefix_table {
            if is_indexed_select_window(action) {
                window_jump_keys.push(key.clone());
            } else if let Some((family, direction)) = directional_family(action) {
                directional.push((family, direction, key));
            } else {
                push_merged(
                    &mut rows,
                    action_name(action),
                    key.clone(),
                    action_label(action),
                );
            }
        }
        if let Some((key, label)) = compact_window_jump_keys(&window_jump_keys) {
            push_merged(&mut rows, "select-window", key, label);
        }
        push_directional(&mut rows, &mut directional);
        rows.sort_by(|a, b| (a.group, a.rank, &a.label).cmp(&(b.group, b.rank, &b.label)));
        Self {
            prefix: cfg.prefix.clone(),
            rows,
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
        }
    }
}

fn action_name(action: &Action) -> &str {
    match action {
        Action::Bare(name) => name,
        Action::Parameterized(p) => &p.action,
    }
}

fn rank_of(name: &str) -> usize {
    ACTION_ORDER
        .iter()
        .position(|n| *n == name)
        .unwrap_or(ACTION_ORDER.len())
}

/// Add `key` to the row already carrying `label`, or start one: two keys
/// bound to the same thing are one row, not two.
fn push_merged(rows: &mut Vec<Row>, name: &str, key: String, label: String) {
    if let Some(row) = rows.iter_mut().find(|r| r.label == label) {
        row.keys.push(key);
        return;
    }
    rows.push(Row {
        group: Group::of(name),
        rank: rank_of(name),
        keys: vec![key],
        label,
    });
}

/// `(index into DIRECTIONAL, index into DIRECTIONS)` for a binding that
/// joins a directional row: the action is in [`DIRECTIONAL`] and carries a
/// cardinal `direction`.
fn directional_family(action: &Action) -> Option<(usize, usize)> {
    let Action::Parameterized(p) = action else {
        return None;
    };
    let family = DIRECTIONAL.iter().position(|(name, _)| *name == p.action)?;
    let direction = p.args.get("direction")?.as_str()?;
    let order = DIRECTIONS.iter().position(|d| *d == direction)?;
    Some((family, order))
}

/// One row per directional family, keys in left/down/up/right order.
fn push_directional(rows: &mut Vec<Row>, bindings: &mut [(usize, usize, &str)]) {
    bindings.sort_unstable();
    for (family, (name, label)) in DIRECTIONAL.iter().enumerate() {
        let keys: Vec<String> = bindings
            .iter()
            .filter(|(f, _, _)| *f == family)
            .map(|(_, _, key)| (*key).to_owned())
            .collect();
        if keys.is_empty() {
            continue;
        }
        rows.push(Row {
            group: Group::of(name),
            rank: rank_of(name),
            keys,
            label: (*label).to_owned(),
        });
    }
}

/// A binding's label as a person reads it (`split right`, `freeze pane`):
/// the action name de-dashed plus its argument values, so new and plugin
/// actions are never unlabelled, with phrases for the few whose raw
/// arguments read backwards (`split-pane`, `move-window`).
fn action_label(action: &Action) -> String {
    let (name, args) = match action {
        Action::Bare(name) => return name.replace('-', " "),
        Action::Parameterized(p) => (p.action.as_str(), &p.args),
    };
    let arg = |key: &str| {
        args.get(key)
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
    };
    match (name, arg("direction").as_deref(), arg("delta").as_deref()) {
        ("split-pane", Some("vertical"), _) => return "split right".to_owned(),
        ("split-pane", Some("horizontal"), _) => return "split below".to_owned(),
        ("move-window", _, Some("-1")) => return "move window left".to_owned(),
        ("move-window", _, Some("1")) => return "move window right".to_owned(),
        ("resize-pane", Some(direction), _) => return format!("resize {direction}"),
        ("focus-direction", Some(direction), _) => return format!("focus {direction}"),
        _ => {}
    }
    if let ("signal-terminal", Some(signal)) = (name, arg("signal")) {
        return format!("{signal} pane");
    }
    let mut label = name.replace('-', " ");
    for value in args.values() {
        label.push(' ');
        label.push_str(
            &value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned),
        );
    }
    label
}

fn is_indexed_select_window(action: &Action) -> bool {
    matches!(
        action,
        Action::Parameterized(p) if p.action == "select-window" && p.args.contains_key("index")
    )
}

/// Collapse sorted numeric window-jump keys into one `0-9` row (a lone key
/// when only one is bound).
fn compact_window_jump_keys(keys: &[String]) -> Option<(String, String)> {
    let first = keys.first()?;
    let last = keys.last()?;
    let key = if keys.len() == 1 {
        first.clone()
    } else {
        format!("{first}-{last}")
    };
    Some((key, "select window".to_owned()))
}

/// Cells between two columns of the grid.
const COLUMN_GAP: usize = 4;
/// The longest label a cell shows before it is clipped.
const LABEL_MAX: usize = 22;
/// Shown in place of the grid when the prefix table is empty.
const EMPTY_NOTICE: &str = "No prefix bindings configured.";

/// One painted line of a column: a group header, a binding, or the blank
/// line that separates two groups sharing a column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cell {
    Header(Group),
    Binding(usize),
    Gap,
}

/// The laid-out legend: columns of cells, and the shared cell widths.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Grid {
    columns: Vec<Vec<Cell>>,
    key_w: usize,
    label_w: usize,
}

impl Grid {
    const fn cell_w(&self) -> usize {
        self.key_w + 2 + self.label_w
    }

    /// Interior width the grid needs.
    const fn width(&self) -> usize {
        let n = self.columns.len();
        n * self.cell_w() + n.saturating_sub(1) * COLUMN_GAP
    }

    fn rows(&self) -> usize {
        self.columns.iter().map(Vec::len).max().unwrap_or(0)
    }
}

/// Flow `groups` (each a header plus its bindings) into columns at most
/// `height` cells tall. With `whole`, a group that does not fit in what is
/// left of a column starts the next one; otherwise a group may continue
/// into the next column, but never leaves its header alone at the bottom.
fn flow(groups: &[(Group, Vec<usize>)], height: usize, whole: bool) -> Vec<Vec<Cell>> {
    let mut columns: Vec<Vec<Cell>> = vec![Vec::new()];
    for (group, rows) in groups {
        let used = columns.last().map_or(0, Vec::len);
        // Below the separating gap: the whole group, or (when it may split)
        // its header and first binding.
        let room = height.saturating_sub(used + 1);
        let need = if whole { 1 + rows.len() } else { 2 };
        let fits = used == 0 || need <= room;
        if !fits {
            columns.push(Vec::new());
        }
        let cells =
            std::iter::once(Cell::Header(*group)).chain(rows.iter().map(|i| Cell::Binding(*i)));
        for (n, cell) in cells.enumerate() {
            let column_len = columns.last().map_or(0, Vec::len);
            if column_len >= height {
                columns.push(Vec::new());
            } else if n == 0 && column_len > 0 {
                columns
                    .last_mut()
                    .into_iter()
                    .for_each(|c| c.push(Cell::Gap));
            }
            columns.last_mut().into_iter().for_each(|c| c.push(cell));
        }
    }
    columns
}

impl WhichKeyOverlay {
    /// The rows of each group, in display order.
    fn groups(&self) -> Vec<(Group, Vec<usize>)> {
        let mut groups: Vec<(Group, Vec<usize>)> = Vec::new();
        for (i, row) in self.rows.iter().enumerate() {
            match groups.last_mut() {
                Some((group, rows)) if *group == row.group => rows.push(i),
                _ => groups.push((row.group, vec![i])),
            }
        }
        groups
    }

    /// The shortest layout whose columns fit the viewport's width: whole
    /// groups per column when the height allows, split groups otherwise.
    fn grid(&self, area: Rect) -> Grid {
        let key_w = self
            .rows
            .iter()
            .map(|row| crate::render::display_width(&row.key_text()))
            .max()
            .unwrap_or(1);
        let label_w = self
            .rows
            .iter()
            .map(|row| crate::render::display_width(&row.label).min(LABEL_MAX))
            .chain(std::iter::once(Group::Session.header().len()))
            .max()
            .unwrap_or(1);
        let mut grid = Grid {
            columns: Vec::new(),
            key_w,
            label_w,
        };
        // Leave the viewport a margin so the panel floats over the work.
        let room = usize::from(area.width)
            .saturating_mul(9)
            .saturating_div(10)
            .saturating_sub(usize::from(2 + super::widgets::MODAL_PAD * 2));
        let max_columns = ((room + COLUMN_GAP) / (grid.cell_w() + COLUMN_GAP)).max(1);
        let max_height = usize::from(area.height.saturating_sub(3)).max(1);
        let groups = self.groups();
        let tallest = groups
            .iter()
            .map(|(_, rows)| 1 + rows.len())
            .max()
            .unwrap_or(1);
        let total: usize = groups.iter().map(|(_, rows)| 2 + rows.len()).sum();
        let shortest = total.div_ceil(max_columns).max(1);
        let fits = |columns: &Vec<Vec<Cell>>| columns.len() <= max_columns;
        grid.columns = (tallest.max(shortest)..=max_height)
            .map(|h| flow(&groups, h, true))
            .find(fits)
            .or_else(|| {
                (shortest..=total)
                    .map(|h| flow(&groups, h, false))
                    .find(fits)
            })
            .unwrap_or_else(|| flow(&groups, total.max(1), false));
        grid
    }

    fn cell_spans(&self, grid: &Grid, cell: Cell) -> Vec<Span<'static>> {
        let blank = || vec![Span::raw(" ".repeat(grid.cell_w()))];
        match cell {
            Cell::Gap => blank(),
            Cell::Header(group) => {
                let pad = grid.label_w.saturating_sub(group.header().len());
                vec![
                    Span::raw(" ".repeat(grid.key_w + 2)),
                    Span::styled(
                        group.header(),
                        Style::default()
                            .fg(self.theme.section_header)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" ".repeat(pad)),
                ]
            }
            Cell::Binding(i) => {
                let Some(row) = self.rows.get(i) else {
                    return blank();
                };
                let keys = row.key_text();
                let key_pad = grid
                    .key_w
                    .saturating_sub(crate::render::display_width(&keys));
                let label = crate::render::clip_text(&row.label, grid.label_w);
                let label_pad = grid
                    .label_w
                    .saturating_sub(crate::render::display_width(&label));
                vec![
                    Span::raw(" ".repeat(key_pad)),
                    Span::styled(
                        keys,
                        Style::default()
                            .fg(self.theme.chord)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(label, Style::default().fg(self.theme.text)),
                    Span::raw(" ".repeat(label_pad)),
                ]
            }
        }
    }

    fn grid_lines(&self, grid: &Grid) -> Vec<Line<'static>> {
        if self.rows.is_empty() {
            return vec![Line::from(Span::styled(
                EMPTY_NOTICE,
                Style::default().fg(self.theme.dim),
            ))];
        }
        (0..grid.rows())
            .map(|r| {
                let mut spans = Vec::new();
                for (c, column) in grid.columns.iter().enumerate() {
                    if c > 0 {
                        spans.push(Span::raw(" ".repeat(COLUMN_GAP)));
                    }
                    let cell = column.get(r).copied().unwrap_or(Cell::Gap);
                    spans.extend(self.cell_spans(grid, cell));
                }
                Line::from(spans)
            })
            .collect()
    }
}

impl RenderOverlay for WhichKeyOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let modal_area = self.bounds(area).unwrap_or(area);
        let body = self.grid_lines(&self.grid(area));
        Modal::new(&self.theme, self.prefix.clone(), body).render_into(modal_area, buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        // Sized to content and anchored to the bottom, above the work the
        // prefix is about to act on.
        let grid = self.grid(area);
        let chrome = 2 + super::widgets::MODAL_PAD * 2;
        let (content_w, content_h) = if self.rows.is_empty() {
            (crate::render::display_width(EMPTY_NOTICE), 1)
        } else {
            (grid.width(), grid.rows())
        };
        let w = u16::try_from(content_w)
            .unwrap_or(u16::MAX)
            .saturating_add(chrome)
            .min(area.width);
        let h = u16::try_from(content_h)
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .min(area.height);
        let x = area.x + (area.width - w) / 2;
        let y = area.y + area.height.saturating_sub(h + 1);
        let rect = Rect::new(x, y, w, h);
        Some(if is_compact(area, self.breakpoints) {
            centered_panel(area, 5, 36, 8, self.breakpoints)
        } else {
            rect
        })
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
    }

    fn handle_key(&mut self, _key: &KeyEvent) -> OverlayCommand {
        // Unreachable: the dispatcher pops passthrough overlays before
        // routing. Dismissing keeps the never-consume invariant anyway.
        OverlayCommand::Dismiss
    }

    fn is_input_passthrough(&self) -> bool {
        true
    }

    fn passthrough_escape_cancels_prefix(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_config::{Action, ParamAction};
    use std::collections::BTreeMap;

    fn cfg_with(prefix: &str, entries: &[(&str, &str)]) -> KeybindingsCfg {
        let prefix_table: BTreeMap<String, Action> = entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Action::Bare((*v).to_owned())))
            .collect();
        KeybindingsCfg {
            prefix: prefix.to_owned(),
            prefix_table,
            ..KeybindingsCfg::default()
        }
    }

    /// `(keys, label)` per row, in display order.
    fn pairs(overlay: &WhichKeyOverlay) -> Vec<(String, String)> {
        overlay
            .rows
            .iter()
            .map(|row| (row.key_text(), row.label.clone()))
            .collect()
    }

    fn render_to_string(overlay: &WhichKeyOverlay, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        overlay.render(area, &mut buf);
        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
            .collect()
    }

    #[test]
    fn rows_reflect_rebound_keys_not_a_hardcoded_table() {
        let overlay = WhichKeyOverlay::from_config(
            &cfg_with("C-a", &[("q", "detach"), ("v", "copy-mode")]),
            &Theme::default(),
        );
        assert!(pairs(&overlay).contains(&("q".to_owned(), "detach".to_owned())));
        assert!(
            !pairs(&overlay).iter().any(|(k, _)| k == "d"),
            "unbound default keys must not appear"
        );
        let text = render_to_string(&overlay, 120, 30);
        assert!(text.contains('q') && text.contains("detach"), "{text}");

        // A rebound prefix shows in the title.
        let overlay = WhichKeyOverlay::from_config(
            &cfg_with("C-Space", &[("d", "detach")]),
            &Theme::default(),
        );
        assert!(render_to_string(&overlay, 80, 24).contains("C-Space"));
    }

    #[test]
    fn parameterized_actions_label_with_args() {
        let mut cfg = cfg_with("C-a", &[]);
        cfg.prefix_table.insert(
            "%".to_owned(),
            param(
                "split-pane",
                &[("direction", toml::Value::String("vertical".to_owned()))],
            ),
        );
        let overlay = WhichKeyOverlay::from_config(&cfg, &Theme::default());
        assert!(
            pairs(&overlay).contains(&("%".to_owned(), "split right".to_owned())),
            "rows: {:?}",
            overlay.rows
        );
    }

    #[test]
    fn numeric_window_jumps_collapse_to_one_row() {
        let mut cfg = cfg_with("C-a", &[("d", "detach")]);
        for i in 0..10u8 {
            cfg.prefix_table.insert(
                i.to_string(),
                param(
                    "select-window",
                    &[("index", toml::Value::Integer(i.into()))],
                ),
            );
        }
        let overlay = WhichKeyOverlay::from_config(&cfg, &Theme::default());
        let rows = pairs(&overlay);
        let collapsed = rows.iter().filter(|(_, a)| a == "select window").count();
        assert_eq!(collapsed, 1);
        assert!(rows.iter().any(|(k, _)| k == "0-9"));
        assert!(!rows.iter().any(|(k, _)| k == "0" || k == "9"));
    }

    #[test]
    fn popup_is_passthrough_and_bounded() {
        use phux_protocol::input::key::{KeyAction, ModSet, PhysicalKey};
        let mut overlay =
            WhichKeyOverlay::from_config(&cfg_with("C-a", &[("d", "detach")]), &Theme::default());
        assert!(overlay.is_input_passthrough());
        let area = Rect::new(0, 0, 80, 24);
        assert!(overlay.bounds(area).is_some(), "floats over the panes");
        // Defensive handle_key dismisses (never consumes).
        let key = KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::A,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        };
        assert_eq!(overlay.handle_key(&key), OverlayCommand::Dismiss);
    }

    #[test]
    fn empty_prefix_table_shows_notice() {
        let overlay = WhichKeyOverlay::from_config(&cfg_with("C-a", &[]), &Theme::default());
        let text = render_to_string(&overlay, 120, 30);
        assert!(
            text.contains("No prefix bindings configured."),
            "empty notice:\n{text}"
        );
    }

    fn param(action: &str, args: &[(&str, toml::Value)]) -> Action {
        Action::Parameterized(ParamAction {
            action: action.to_owned(),
            args: args
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect(),
        })
    }

    /// Labels read as phrases a person says, not as call syntax.
    #[test]
    fn labels_read_as_phrases() {
        let s = |v: &str| toml::Value::String(v.to_owned());
        assert_eq!(
            action_label(&Action::Bare("kill-pane".to_owned())),
            "kill pane"
        );
        assert_eq!(
            action_label(&param(
                "resize-pane",
                &[
                    ("direction", s("left")),
                    ("amount", toml::Value::Integer(5))
                ]
            )),
            "resize left"
        );
        assert_eq!(
            action_label(&param(
                "move-window",
                &[("delta", toml::Value::Integer(-1))]
            )),
            "move window left"
        );
        assert_eq!(
            action_label(&param("signal-terminal", &[("signal", s("freeze"))])),
            "freeze pane"
        );
        assert_eq!(
            action_label(&param("plugin-action", &[("id", s("summarize"))])),
            "plugin action summarize"
        );
    }

    /// On a roomy screen the grid shows every binding: nothing is cut off
    /// the bottom of a single column.
    #[test]
    fn every_binding_fits_on_a_roomy_screen() {
        let entries: Vec<(String, String)> = (b'a'..=b'z')
            .chain(b'A'..=b'P')
            .map(|c| ((c as char).to_string(), format!("action-{}", c as char)))
            .collect();
        let refs: Vec<(&str, &str)> = entries
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let overlay = WhichKeyOverlay::from_config(&cfg_with("C-a", &refs), &Theme::default());
        let text = render_to_string(&overlay, 160, 40);
        for (_, action) in &entries {
            let label = action.replace('-', " ");
            assert!(text.contains(&label), "{label} missing:\n{text}");
        }
    }

    /// Two keys for one action are one row; a directional family is one row
    /// with its keys in left/down/up/right order.
    #[test]
    fn duplicate_and_directional_bindings_share_a_row() {
        let s = |v: &str| toml::Value::String(v.to_owned());
        let mut cfg = cfg_with(
            "C-a",
            &[("Space", "command-palette"), (":", "command-palette")],
        );
        for (key, direction) in [("l", "right"), ("h", "left"), ("k", "up"), ("j", "down")] {
            cfg.prefix_table.insert(
                key.to_owned(),
                param("focus-direction", &[("direction", s(direction))]),
            );
        }
        let rows = pairs(&WhichKeyOverlay::from_config(&cfg, &Theme::default()));
        assert!(
            rows.contains(&(": Space".to_owned(), "command palette".to_owned())),
            "{rows:?}"
        );
        assert!(
            rows.contains(&("h j k l".to_owned(), "focus".to_owned())),
            "{rows:?}"
        );
        assert_eq!(rows.len(), 2, "{rows:?}");
    }

    /// Bindings group under Pane / Window / Session / phux headers, in that
    /// order, whatever order the keys sort in.
    #[test]
    fn bindings_render_under_group_headers() {
        let overlay = WhichKeyOverlay::from_config(
            &cfg_with(
                "C-a",
                &[
                    ("d", "detach"),
                    ("x", "kill-pane"),
                    ("c", "new-window"),
                    ("S", "settings"),
                ],
            ),
            &Theme::default(),
        );
        let groups: Vec<Group> = overlay.rows.iter().map(|r| r.group).collect();
        assert_eq!(
            groups,
            [Group::Pane, Group::Window, Group::Session, Group::Phux]
        );
        let text = render_to_string(&overlay, 160, 40);
        for header in ["Pane", "Window", "Session", "phux"] {
            assert!(text.contains(header), "{header} missing:\n{text}");
        }
    }

    /// The shipped prefix table lays out one group per column on a roomy
    /// screen, shows every binding on a modest one, and stays inside a
    /// classic 80x24.
    #[test]
    fn shipped_table_fits_roomy_and_classic_screens() {
        let cfg = phux_config::parse_str(
            phux_config::DEFAULT_CONFIG_TOML,
            std::path::Path::new("default.toml"),
        )
        .expect("shipped config parses")
        .keybindings;
        let overlay = WhichKeyOverlay::from_config(&cfg, &Theme::default());
        let roomy = overlay.grid(Rect::new(0, 0, 160, 45));
        assert_eq!(roomy.columns.len(), 4, "{roomy:?}");
        for (column, cells) in roomy.columns.iter().enumerate() {
            assert!(
                matches!(cells.first(), Some(Cell::Header(_))),
                "column {column}: {cells:?}"
            );
        }
        let classic = Rect::new(0, 0, 80, 24);
        let bounds = overlay.bounds(classic).expect("floats");
        assert!(bounds.height <= classic.height && bounds.width <= classic.width);
        let text = render_to_string(&overlay, 120, 30);
        for row in &overlay.rows {
            let label = crate::render::clip_text(&row.label, LABEL_MAX);
            assert!(text.contains(&label), "{label} missing:\n{text}");
        }
    }
}
