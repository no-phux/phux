//! Yes/no confirmation modals: Enter or `y` returns the held command, Esc or
//! `n` drops it.
//!
//! - Paste protection: a paste that would reach a pane as typed input.
//! - Clipboard writes under `defaults.clipboard-write = "ask"` (ADR-0158).
//! - `kill-window` / `kill-pane`: names how many terminals end and which are
//!   also shown elsewhere, so closing a view is never mistaken for killing;
//!   the commit kills the exact set shown.

use std::collections::BTreeMap;

use phux_client_core::engine::ClipboardText;
use phux_config::keybind::ResolvedAction;
use phux_protocol::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, PhysicalKey};
use phux_protocol::input::paste::PasteEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

use super::widgets::{Modal, centered_panel};
use super::{OverlayCommand, RenderOverlay};
use crate::render::{ChromeBreakpoints, Theme};

/// The arg `kill-window` and `kill-pane` carry once confirmed: the selectors
/// of exactly the terminals the confirm named.
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

/// Holds one command until the user confirms or drops it.
#[derive(Debug, Clone)]
pub struct ConfirmOverlay {
    title: &'static str,
    lines: Vec<String>,
    legend: &'static str,
    on_yes: OverlayCommand,
    theme: Theme,
    breakpoints: ChromeBreakpoints,
}

impl ConfirmOverlay {
    fn new(
        title: &'static str,
        lines: Vec<String>,
        legend: &'static str,
        on_yes: OverlayCommand,
        theme: &Theme,
    ) -> Self {
        Self {
            title,
            lines,
            legend,
            on_yes,
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
        }
    }

    /// Ask before delivering `paste` of `lines` lines; `bracketed` is the
    /// pane's DEC 2004 state, which decides what the warning says.
    #[must_use]
    pub fn paste(paste: PasteEvent, lines: usize, bracketed: bool, theme: &Theme) -> Self {
        let message = if bracketed {
            "this paste could end bracketed paste early".to_owned()
        } else if lines == 1 {
            "the line break will press Enter".to_owned()
        } else {
            format!("{lines} lines will run as typed input")
        };
        let on_yes = OverlayCommand::Paste(paste);
        Self::new("Paste?", vec![message], "⏎ paste · esc", on_yes, theme)
    }

    /// Ask before `source` (a pane label) sets the clipboard to `text`.
    #[must_use]
    pub fn clipboard(source: &str, text: ClipboardText, theme: &Theme) -> Self {
        let message = format!(
            "{source} wants to set the clipboard ({} bytes)",
            text.0.len()
        );
        let on_yes = OverlayCommand::SetClipboard(text);
        Self::new(
            "Clipboard?",
            vec![message],
            "⏎ allow · esc deny",
            on_yes,
            theme,
        )
    }

    /// Ask before terminating `targets`, the terminals of window `window`;
    /// `shared` names those also shown elsewhere.
    #[must_use]
    pub fn kill_window(
        window: &str,
        targets: &[ResourceId],
        shared: &[SharedTarget],
        theme: &Theme,
    ) -> Self {
        let count = targets.len();
        let noun = if count == 1 { "terminal" } else { "terminals" };
        let line = if window.is_empty() {
            format!("End {count} {noun} in this tab.")
        } else {
            format!("End {count} {noun} in tab {window}.")
        };
        Self::kill("kill-window", "Kill tab?", targets, line, shared, theme)
    }

    /// Ask before terminating `target`, the focused pane's terminal, named
    /// `label` (e.g. `pane 3`); `shared` names it when shown elsewhere.
    #[must_use]
    pub fn kill_pane(
        label: &str,
        target: &ResourceId,
        shared: &[SharedTarget],
        theme: &Theme,
    ) -> Self {
        let line = format!("End {label} and what runs in it.");
        let targets = std::slice::from_ref(target);
        Self::kill("kill-pane", "Kill pane?", targets, line, shared, theme)
    }

    fn kill(
        action: &str,
        title: &'static str,
        targets: &[ResourceId],
        first: String,
        shared: &[SharedTarget],
        theme: &Theme,
    ) -> Self {
        let mut lines = vec![first];
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
        let selectors = targets
            .iter()
            .map(|id| toml::Value::String(selector(id)))
            .collect();
        let on_yes = OverlayCommand::Commit(ResolvedAction {
            action: action.to_owned(),
            args: BTreeMap::from([(KILL_TARGETS_ARG.to_owned(), toml::Value::Array(selectors))]),
        });
        Self::new(title, lines, "⏎ kill · esc", on_yes, theme)
    }

    /// The lines the modal shows, top to bottom.
    #[cfg(test)]
    pub(crate) fn lines(&self) -> &[String] {
        &self.lines
    }

    fn modal_area(&self, outer: Rect) -> Rect {
        lines_modal_area(outer, self.lines.len(), self.breakpoints)
    }
}

/// A half-width modal exactly tall enough for `lines` lines, centered.
pub(crate) fn lines_modal_area(outer: Rect, lines: usize, bp: ChromeBreakpoints) -> Rect {
    let rows = u16::try_from(lines).unwrap_or(u16::MAX).saturating_add(2);
    let wide = centered_panel(outer, 5, 20, rows, bp);
    let h = rows.min(outer.height);
    let y = outer.y + (outer.height.saturating_sub(h)) / 2;
    Rect::new(wide.x, y, wide.width, h)
}

/// The canonical selector of `id`: `@N`, or `host/@N` for a satellite.
pub(crate) fn selector(id: &ResourceId) -> String {
    match id {
        ResourceId::Local { id } => format!("@{id}"),
        ResourceId::Satellite { host, id } => format!("{host}/@{id}"),
    }
}

impl RenderOverlay for ConfirmOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let text = Style::default().fg(self.theme.text);
        let lines = self
            .lines
            .iter()
            .map(|line| Line::styled(line.clone(), text))
            .collect();
        Modal::new(&self.theme, self.title.to_owned(), lines)
            .footer("", self.legend)
            .render_into(self.modal_area(area), buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(self.modal_area(area))
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        match key.key {
            PhysicalKey::Enter | PhysicalKey::Y => self.on_yes.clone(),
            PhysicalKey::Escape | PhysicalKey::N => OverlayCommand::Dismiss,
            _ => OverlayCommand::Stay,
        }
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::input::paste::PasteTrust;

    use super::*;
    use crate::test_keys::press;

    fn paste(data: &[u8]) -> PasteEvent {
        PasteEvent {
            trust: PasteTrust::Trusted,
            data: data.to_vec(),
        }
    }

    fn clipboard() -> ConfirmOverlay {
        let text = ClipboardText("secret".to_owned());
        ConfirmOverlay::clipboard("pane 3", text, &Theme::default())
    }

    fn kill_window(shared: &[SharedTarget]) -> ConfirmOverlay {
        let targets = vec![
            ResourceId::local(1),
            ResourceId::satellite(SatelliteHost::new("edge"), 9),
        ];
        ConfirmOverlay::kill_window("build", &targets, shared, &Theme::default())
    }

    fn kill_args(command: OverlayCommand) -> (String, Option<toml::Value>) {
        let OverlayCommand::Commit(action) = command else {
            panic!("commit");
        };
        (action.action, action.args.get(KILL_TARGETS_ARG).cloned())
    }

    #[test]
    fn enter_or_y_returns_the_held_command_and_esc_or_n_drops_it() {
        let held = paste(b"make\nmake install\n");
        let paste_confirm = ConfirmOverlay::paste(held.clone(), 2, false, &Theme::default());
        for key in [PhysicalKey::Enter, PhysicalKey::Y] {
            assert_eq!(
                paste_confirm.clone().handle_key(&press(key)),
                OverlayCommand::Paste(held.clone())
            );
            assert_eq!(
                clipboard().handle_key(&press(key)),
                OverlayCommand::SetClipboard(ClipboardText("secret".to_owned()))
            );
        }
        for key in [PhysicalKey::Escape, PhysicalKey::N] {
            assert_eq!(
                paste_confirm.clone().handle_key(&press(key)),
                OverlayCommand::Dismiss
            );
            assert_eq!(
                kill_window(&[]).handle_key(&press(key)),
                OverlayCommand::Dismiss
            );
        }
        assert_eq!(
            kill_window(&[]).handle_key(&press(PhysicalKey::A)),
            OverlayCommand::Stay
        );
    }

    #[test]
    fn the_paste_message_names_what_will_happen() {
        let theme = Theme::default();
        let lines =
            |n, bracketed| ConfirmOverlay::paste(paste(b"ls\n"), n, bracketed, &theme).lines;
        assert_eq!(lines(2, false), ["2 lines will run as typed input"]);
        assert_eq!(lines(1, false), ["the line break will press Enter"]);
        assert_eq!(
            lines(1, true),
            ["this paste could end bracketed paste early"]
        );
    }

    #[test]
    fn the_clipboard_message_hides_the_text() {
        assert_eq!(
            clipboard().lines,
            ["pane 3 wants to set the clipboard (6 bytes)"]
        );
        assert!(!format!("{:?}", clipboard()).contains("secret"));
    }

    #[test]
    fn a_kill_commits_its_exact_targets() {
        for key in [PhysicalKey::Enter, PhysicalKey::Y] {
            assert_eq!(
                kill_args(kill_window(&[]).handle_key(&press(key))),
                (
                    "kill-window".to_owned(),
                    Some(toml::Value::Array(vec![
                        toml::Value::String("@1".to_owned()),
                        toml::Value::String("edge/@9".to_owned()),
                    ]))
                )
            );
        }
        let mut pane =
            ConfirmOverlay::kill_pane("pane 3", &ResourceId::local(3), &[], &Theme::default());
        assert_eq!(pane.lines, ["End pane 3 and what runs in it."]);
        assert_eq!(
            kill_args(pane.handle_key(&press(PhysicalKey::Y))),
            (
                "kill-pane".to_owned(),
                Some(toml::Value::Array(vec![toml::Value::String(
                    "@3".to_owned()
                )]))
            )
        );
    }

    #[test]
    fn the_kill_message_counts_terminals_and_names_shared_ones() {
        assert_eq!(kill_window(&[]).lines, ["End 2 terminals in tab build."]);
        let shared = SharedTarget {
            label: "pane 1".to_owned(),
            elsewhere: vec!["window logs".to_owned(), "session notes".to_owned()],
        };
        assert_eq!(
            kill_window(std::slice::from_ref(&shared)).lines[1],
            "pane 1 is also shown in window logs, session notes"
        );
        let lines = kill_window(&vec![shared; 6]).lines;
        assert_eq!(lines.len(), 1 + 4 + 1);
        assert_eq!(lines[5], "2 more shown elsewhere");
    }
}
