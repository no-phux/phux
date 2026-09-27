//! Which-key popup.
//!
//! After the prefix and `which-key-delay-ms` of hesitation, a floating legend of every prefix-table continuation from the live
//! [`KeybindingsCfg`] (numeric window jumps collapse to `0-9`).
//!
//! It is transparent to input ([`RenderOverlay::is_input_passthrough`]): the
//! next key dismisses it and runs as if it had never appeared (Esc also
//! cancels the prefix), so it can never eat or delay a chord.

use phux_config::{Action, KeybindingsCfg};
use phux_protocol::input::key::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::widgets::{Modal, centered_panel, is_compact};
use super::{OverlayCommand, RenderOverlay};
use crate::render::{ChromeBreakpoints, Theme};

/// Which-key popup: prefix-table continuations as `key  action` rows.
#[derive(Debug)]
pub struct WhichKeyOverlay {
    /// Pretty-printed prefix chord as authored in config (e.g. `"C-a"`),
    /// shown in the title so the user sees which pending prefix this is.
    prefix: String,
    /// `(key, action)` rows in prefix-table order (`BTreeMap` iteration,
    /// so stable across opens).
    rows: Vec<(String, String)>,
    /// Color slots snapshotted from the active [`Theme`] at construction.
    theme: Theme,
    /// `[chrome]` thresholds, stamped by `OverlayState::push`.
    breakpoints: ChromeBreakpoints,
}

impl WhichKeyOverlay {
    /// Build the popup from the live config snapshot, styled with `theme`.
    #[must_use]
    pub fn from_config(cfg: &KeybindingsCfg, theme: &Theme) -> Self {
        let mut rows: Vec<(String, String)> = Vec::new();
        let mut window_jump_keys: Vec<String> = Vec::new();
        for (key, action) in &cfg.prefix_table {
            if is_indexed_select_window(action) {
                window_jump_keys.push(key.clone());
            } else {
                rows.push((key.clone(), action_label(action)));
            }
        }
        if let Some(row) = compact_window_jump_keys(&window_jump_keys) {
            rows.push(row);
        }
        Self {
            prefix: cfg.prefix.clone(),
            rows,
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
        }
    }
}

/// A binding's label as a person reads it (`split right`, `resize pane left
/// 5`): the action name de-dashed plus its argument values, so new and
/// plugin actions are never unlabelled, with phrases for the few whose raw
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

/// The grid the bindings flow into: how many columns, how many rows, and
/// how wide each cell is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Grid {
    columns: usize,
    rows: usize,
    key_w: usize,
    label_w: usize,
}

impl Grid {
    const fn cell_w(self) -> usize {
        self.key_w + 2 + self.label_w
    }

    /// Interior width the grid needs.
    const fn width(self) -> usize {
        self.columns * self.cell_w() + (self.columns.saturating_sub(1)) * COLUMN_GAP
    }
}

impl WhichKeyOverlay {
    /// Fit the rows into the fewest rows that the viewport's width allows,
    /// column-major so the keys read down each column in order.
    fn grid(&self, area: Rect) -> Grid {
        let key_w = self
            .rows
            .iter()
            .map(|(key, _)| crate::render::display_width(key))
            .max()
            .unwrap_or(1);
        let label_w = self
            .rows
            .iter()
            .map(|(_, label)| crate::render::display_width(label).min(LABEL_MAX))
            .max()
            .unwrap_or(1);
        // Leave the viewport a margin so the panel floats over the work.
        let room = usize::from(area.width)
            .saturating_mul(9)
            .saturating_div(10)
            .saturating_sub(usize::from(2 + super::widgets::MODAL_PAD * 2));
        let one = Grid {
            columns: 1,
            rows: self.rows.len().max(1),
            key_w,
            label_w,
        };
        let columns = (room + COLUMN_GAP) / (one.cell_w() + COLUMN_GAP);
        let columns = columns.clamp(1, self.rows.len().max(1));
        Grid {
            columns,
            rows: self.rows.len().div_ceil(columns).max(1),
            ..one
        }
    }

    fn grid_lines(&self, grid: Grid) -> Vec<ratatui::text::Line<'static>> {
        use ratatui::style::{Modifier, Style};
        use ratatui::text::{Line, Span};
        if self.rows.is_empty() {
            return vec![Line::from(Span::styled(
                EMPTY_NOTICE,
                Style::default().fg(self.theme.dim),
            ))];
        }
        (0..grid.rows)
            .map(|r| {
                let mut spans = Vec::new();
                for c in 0..grid.columns {
                    let Some((key, label)) = self.rows.get(c * grid.rows + r) else {
                        break;
                    };
                    if c > 0 {
                        spans.push(Span::raw(" ".repeat(COLUMN_GAP)));
                    }
                    let key_pad = grid.key_w.saturating_sub(crate::render::display_width(key));
                    let label = crate::render::clip_text(label, grid.label_w);
                    let label_pad = grid
                        .label_w
                        .saturating_sub(crate::render::display_width(&label));
                    spans.push(Span::raw(" ".repeat(key_pad)));
                    spans.push(Span::styled(
                        key.clone(),
                        Style::default()
                            .fg(self.theme.chord)
                            .add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(label, Style::default().fg(self.theme.text)));
                    spans.push(Span::raw(" ".repeat(label_pad)));
                }
                Line::from(spans)
            })
            .collect()
    }
}

impl RenderOverlay for WhichKeyOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let modal_area = self.bounds(area).unwrap_or(area);
        let body = self.grid_lines(self.grid(area));
        Modal::new(&self.theme, self.prefix.clone(), body).render_into(modal_area, buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        // Sized to content and anchored to the bottom, above the work the
        // prefix is about to act on.
        let grid = self.grid(area);
        let chrome = 2 + super::widgets::MODAL_PAD * 2;
        let content_w = if self.rows.is_empty() {
            crate::render::display_width(EMPTY_NOTICE)
        } else {
            grid.width()
        };
        let w = u16::try_from(content_w)
            .unwrap_or(u16::MAX)
            .saturating_add(chrome)
            .min(area.width);
        let h = u16::try_from(grid.rows)
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
        assert!(
            overlay
                .rows
                .contains(&("q".to_owned(), "detach".to_owned()))
        );
        assert!(
            !overlay.rows.iter().any(|(k, _)| k == "d"),
            "unbound default keys must not appear"
        );
        let text = render_to_string(&overlay, 80, 24);
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
            overlay
                .rows
                .contains(&("%".to_owned(), "split right".to_owned())),
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
        let collapsed = overlay
            .rows
            .iter()
            .filter(|(_, a)| a == "select window")
            .count();
        assert_eq!(collapsed, 1);
        assert!(overlay.rows.iter().any(|(k, _)| k == "0-9"));
        assert!(!overlay.rows.iter().any(|(k, _)| k == "0" || k == "9"));
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
        let text = render_to_string(&overlay, 80, 24);
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
}
