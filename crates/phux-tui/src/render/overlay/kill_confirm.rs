//! Kill confirmation: the modal `kill-window` raises before it terminates a
//! window's terminals.
//!
//! It names how many terminals end and which of them are also shown
//! elsewhere (another window, another session's arrangement), so closing a
//! tab is never mistaken for closing a view. Enter or `y` commits
//! `kill-window` with the exact target set it showed; Esc or `n` drops it.

use std::collections::BTreeMap;

use phux_config::keybind::ResolvedAction;
use phux_protocol::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, PhysicalKey};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

use super::widgets::{Modal, centered_panel};
use super::{OverlayCommand, RenderOverlay};
use crate::render::{ChromeBreakpoints, Theme};

/// The arg `kill-window` carries once confirmed: the selectors of exactly
/// the terminals the confirm named.
pub const KILL_TARGETS_ARG: &str = "terminals";

/// Most "also shown" lines listed before the rest are counted.
const MAX_SHARED_LINES: usize = 4;

/// One terminal the kill would end that another view also shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedTarget {
    /// The terminal's label, e.g. `pane 3`.
    pub label: String,
    /// Where else it is shown, e.g. `window build`, `session notes`.
    pub elsewhere: Vec<String>,
}

/// Holds one `kill-window` until the user confirms or drops it.
#[derive(Debug, Clone)]
pub struct KillConfirmOverlay {
    targets: Vec<ResourceId>,
    lines: Vec<String>,
    theme: Theme,
    breakpoints: ChromeBreakpoints,
}

impl KillConfirmOverlay {
    /// Ask before terminating `targets` (selectors from `selector`), the
    /// terminals of window `window`; `shared` names those also shown
    /// elsewhere.
    #[must_use]
    pub fn new(
        window: &str,
        targets: Vec<ResourceId>,
        shared: &[SharedTarget],
        theme: &Theme,
    ) -> Self {
        let count = targets.len();
        let noun = if count == 1 { "terminal" } else { "terminals" };
        let mut lines = vec![if window.is_empty() {
            format!("End {count} {noun} in this window.")
        } else {
            format!("End {count} {noun} in window {window}.")
        }];
        for target in shared.iter().take(MAX_SHARED_LINES) {
            lines.push(format!(
                "{} is also shown in {}",
                target.label,
                target.elsewhere.join(", ")
            ));
        }
        if shared.len() > MAX_SHARED_LINES {
            lines.push(format!(
                "{} more shown elsewhere",
                shared.len() - MAX_SHARED_LINES
            ));
        }
        lines.push("Enter kill  ·  Esc cancel".to_owned());
        Self {
            targets,
            lines,
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
        }
    }

    /// The lines the modal shows, top to bottom.
    #[must_use]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// The `kill-window` this confirm commits: the exact target set it named.
    #[must_use]
    pub fn committed_action(&self) -> ResolvedAction {
        let selectors = self
            .targets
            .iter()
            .map(|id| toml::Value::String(selector(id)))
            .collect();
        ResolvedAction {
            action: "kill-window".to_owned(),
            args: BTreeMap::from([(KILL_TARGETS_ARG.to_owned(), toml::Value::Array(selectors))]),
        }
    }

    fn modal_area(&self, outer: Rect, bp: ChromeBreakpoints) -> Rect {
        let rows = u16::try_from(self.lines.len()).unwrap_or(u16::MAX);
        let wide = centered_panel(outer, 5, 20, rows.saturating_add(2), bp);
        let h = rows.saturating_add(2).min(outer.height);
        let y = outer.y + (outer.height.saturating_sub(h)) / 2;
        Rect::new(wide.x, y, wide.width, h)
    }
}

/// The canonical selector of `id`: `@N`, or `host/@N` for a satellite.
fn selector(id: &ResourceId) -> String {
    match id {
        ResourceId::Local { id } => format!("@{id}"),
        ResourceId::Satellite { host, id } => format!("{host}/@{id}"),
    }
}

impl RenderOverlay for KillConfirmOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let text = Style::default().fg(self.theme.text);
        let lines = self
            .lines
            .iter()
            .map(|line| Line::styled(line.clone(), text))
            .collect();
        Modal::new(&self.theme, "kill?".to_owned(), lines)
            .render_into(self.modal_area(area, self.breakpoints), buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(self.modal_area(area, self.breakpoints))
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        match key.key {
            PhysicalKey::Enter | PhysicalKey::Y => OverlayCommand::Commit(self.committed_action()),
            PhysicalKey::Escape | PhysicalKey::N => OverlayCommand::Dismiss,
            _ => OverlayCommand::Stay,
        }
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::input::key::ModSet;

    use super::*;

    fn press(key: PhysicalKey) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: None,
            unshifted_codepoint: None,
        }
    }

    fn overlay(shared: &[SharedTarget]) -> KillConfirmOverlay {
        KillConfirmOverlay::new(
            "build",
            vec![
                ResourceId::local(1),
                ResourceId::satellite(SatelliteHost::new("edge"), 9),
            ],
            shared,
            &Theme::default(),
        )
    }

    #[test]
    fn enter_or_y_commits_kill_window_with_the_exact_targets() {
        for key in [PhysicalKey::Enter, PhysicalKey::Y] {
            let OverlayCommand::Commit(action) = overlay(&[]).handle_key(&press(key)) else {
                panic!("commit");
            };
            assert_eq!(action.action, "kill-window");
            assert_eq!(
                action.args.get(KILL_TARGETS_ARG),
                Some(&toml::Value::Array(vec![
                    toml::Value::String("@1".to_owned()),
                    toml::Value::String("edge/@9".to_owned()),
                ]))
            );
        }
    }

    #[test]
    fn escape_or_n_cancels_and_other_keys_wait() {
        for key in [PhysicalKey::Escape, PhysicalKey::N] {
            assert_eq!(
                overlay(&[]).handle_key(&press(key)),
                OverlayCommand::Dismiss
            );
        }
        assert_eq!(
            overlay(&[]).handle_key(&press(PhysicalKey::A)),
            OverlayCommand::Stay
        );
    }

    #[test]
    fn the_message_counts_terminals_and_names_shared_ones() {
        assert_eq!(
            overlay(&[]).lines(),
            [
                "End 2 terminals in window build.",
                "Enter kill  ·  Esc cancel"
            ]
        );
        let shared = SharedTarget {
            label: "pane 1".to_owned(),
            elsewhere: vec!["window logs".to_owned(), "session notes".to_owned()],
        };
        assert_eq!(
            overlay(std::slice::from_ref(&shared)).lines()[1],
            "pane 1 is also shown in window logs, session notes"
        );
        let many = vec![shared; 6];
        let lines = overlay(&many).lines().to_vec();
        assert_eq!(lines.len(), 1 + 4 + 1 + 1);
        assert_eq!(lines[5], "2 more shown elsewhere");
    }
}
