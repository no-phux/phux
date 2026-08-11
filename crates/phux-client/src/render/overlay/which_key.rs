//! Which-key popup (phux-foz.2).
//!
//! A small floating panel that appears when the user presses the prefix
//! and then hesitates: it lists every prefix-table continuation (key,
//! action) so the next keystroke can be *discovered* instead of
//! memorized. The driver pushes it after `which-key-delay-ms` of prefix
//! inactivity (see `[keybindings]` in the config).
//!
//! Unlike every other overlay, the popup is **transparent to input**
//! ([`RenderOverlay::is_input_passthrough`] returns `true`): the
//! dispatcher never routes keys to it. Any subsequent key dismisses the
//! popup and then executes exactly as if the popup had never appeared —
//! the pending prefix chord stays live in the resolver — and Esc
//! dismisses it while also cancelling the prefix. The popup therefore
//! can never eat or delay a chord; it is a pure display layer over the
//! resolver's pending state.
//!
//! Rows are built from the same [`KeybindingsCfg`] snapshot the help
//! overlay uses, so user rebinds (and removed defaults) are reflected
//! exactly. Numeric window-jump bindings collapse into a single
//! `0-9  select window by number` row, matching the help overlay.

use phux_config::KeybindingsCfg;
use phux_protocol::input::key::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::help::{action_label, is_indexed_select_window};
use super::widgets::{ChordRow, ChordSection, KeyChordTable, Modal, centered_panel};
use super::{OverlayCommand, RenderOverlay};
use crate::render::Theme;

/// Which-key popup: prefix-table continuations as `key  action` rows.
///
/// Built from a [`KeybindingsCfg`] snapshot via [`Self::from_config`];
/// owns its strings and theme so the boxed overlay stays `'static`.
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
}

impl WhichKeyOverlay {
    /// Build the popup from a config snapshot, styled with `theme`.
    ///
    /// Sources the SAME data as the help overlay ([`KeybindingsCfg`]),
    /// so rebound keys show the user's actual bindings. The numeric
    /// `select-window { index }` keys collapse into one row.
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
        }
    }
}

/// Collapse the numeric window-jump keys into a single `0-9` row (or a
/// lone `0` when only one is bound). `keys` arrive sorted (`BTreeMap`
/// iteration). `None` when no such keys exist.
fn compact_window_jump_keys(keys: &[String]) -> Option<(String, String)> {
    let first = keys.first()?;
    let last = keys.last()?;
    let key = if keys.len() == 1 {
        first.clone()
    } else {
        format!("{first}-{last}")
    };
    Some((key, "select window by number".to_owned()))
}

impl RenderOverlay for WhichKeyOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let modal_area = self.bounds(area).unwrap_or(area);
        let rows = self
            .rows
            .iter()
            .map(|(key, action)| ChordRow::new(key.clone(), action.clone()))
            .collect::<Vec<_>>();
        let body = KeyChordTable::new(
            &self.theme,
            vec![ChordSection::new(
                format!("{} continuations", self.prefix),
                rows,
            )],
        )
        .empty_notice("No prefix bindings configured.")
        .body_lines();
        Modal::new(&self.theme, format!("{} ...", self.prefix), body)
            .footer("Esc cancels the prefix; any other key runs its binding")
            .wrap(true)
            .render_into(modal_area, buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        // Same floating-modal shape as help, slightly smaller: ~60% of
        // the viewport, min 36x8, clamped to the outer rect.
        Some(centered_panel(area, 6, 36, 8))
    }

    fn handle_key(&mut self, _key: &KeyEvent) -> OverlayCommand {
        // Defensive only: the dispatcher intercepts input BEFORE overlay
        // routing for passthrough overlays (it pops the popup and lets
        // the key execute normally), so this is unreachable in the real
        // input path. If some future path routes here anyway, dismissing
        // preserves the invariant that the popup never consumes input.
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
    fn rows_reflect_rebound_keys_not_a_hardcoded_table() {
        // A user who rebound detach to `q` (and never bound `d`) must see
        // `q  detach` — the popup sources the live config snapshot, not a
        // baked-in default table.
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
        assert!(text.contains('q'), "rebound key row:\n{text}");
        assert!(text.contains("detach"), "action label:\n{text}");
    }

    #[test]
    fn title_shows_the_configured_prefix() {
        // Rebinding the prefix itself must show up in the popup title.
        let overlay = WhichKeyOverlay::from_config(
            &cfg_with("C-Space", &[("d", "detach")]),
            &Theme::default(),
        );
        let text = render_to_string(&overlay, 80, 24);
        assert!(text.contains("C-Space ..."), "title:\n{text}");
    }

    #[test]
    fn parameterized_actions_label_with_args() {
        let mut args = BTreeMap::new();
        args.insert(
            "direction".to_owned(),
            toml::Value::String("vertical".to_owned()),
        );
        let mut cfg = cfg_with("C-a", &[]);
        cfg.prefix_table.insert(
            "%".to_owned(),
            Action::Parameterized(ParamAction {
                action: "split-pane".to_owned(),
                args,
            }),
        );
        let overlay = WhichKeyOverlay::from_config(&cfg, &Theme::default());
        assert!(
            overlay
                .rows
                .contains(&("%".to_owned(), "split-pane(direction=vertical)".to_owned())),
            "rows: {:?}",
            overlay.rows
        );
    }

    #[test]
    fn numeric_window_jumps_collapse_to_one_row() {
        let mut cfg = cfg_with("C-a", &[("d", "detach")]);
        for i in 0..10u8 {
            let mut args = BTreeMap::new();
            args.insert("index".to_owned(), toml::Value::Integer(i.into()));
            cfg.prefix_table.insert(
                i.to_string(),
                Action::Parameterized(ParamAction {
                    action: "select-window".to_owned(),
                    args,
                }),
            );
        }
        let overlay = WhichKeyOverlay::from_config(&cfg, &Theme::default());
        let collapsed = overlay
            .rows
            .iter()
            .filter(|(_, a)| a == "select window by number")
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
}
