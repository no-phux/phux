//! Yes/no confirmation modals on ratcn's [`Dialog`].
//!
//! Enter presses the focused button (focus starts on the confirming one),
//! `y` confirms and Esc or `n` drops the held command from anywhere; Tab,
//! arrows, and the mouse move between and press the buttons.
//!
//! - Paste protection: a paste that would reach a pane as typed input.
//! - Clipboard writes under `defaults.clipboard-write = "ask"` (ADR-0158).
//! - `kill-window` / `kill-pane`: names how many terminals end and which are
//!   also shown elsewhere, so closing a view is never mistaken for killing;
//!   the commit kills the exact set shown.

use std::cell::Cell;
use std::collections::BTreeMap;

use phux_client_core::engine::ClipboardText;
use phux_protocol::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, PhysicalKey};
use phux_protocol::input::mouse::MouseEvent;
use phux_protocol::input::paste::PasteEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratcn::runtime::{DeclareCtx, Event, EventResult, FocusState, Ratcn};
use ratcn::{Button, Dialog};

use super::widgets::centered_panel;
use super::{OverlayCommand, RenderOverlay};
use crate::render::kit::{self, Surface};
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

/// What a confirm's ratcn surface tracks between frames: which button has
/// focus.
#[derive(Debug)]
struct Model {
    focus: FocusState,
}

/// What the dialog's buttons and focus traversal ask for.
#[derive(Debug, Clone)]
enum Msg {
    Focus(FocusState),
    Yes,
    No,
}

const DIALOG: &str = "confirm";
const YES: &str = "yes";
const NO: &str = "no";

/// The two buttons: confirming label, declining label, and whether
/// confirming destroys something (the button paints destructive).
#[derive(Debug, Clone, Copy)]
struct Buttons {
    yes: &'static str,
    no: &'static str,
    destructive: bool,
}

/// Holds one command until the user confirms or drops it.
pub struct ConfirmOverlay {
    title: &'static str,
    lines: Vec<String>,
    buttons: Buttons,
    on_yes: OverlayCommand,
    theme: Theme,
    breakpoints: ChromeBreakpoints,
    model: Model,
    ui: Surface<Model, Msg>,
    /// The viewport of the last paint, which input is laid out against.
    outer: Cell<Rect>,
}

impl std::fmt::Debug for ConfirmOverlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `on_yes` is elided: it can hold clipboard text.
        f.debug_struct("ConfirmOverlay")
            .field("title", &self.title)
            .field("lines", &self.lines)
            .field("buttons", &self.buttons)
            .finish_non_exhaustive()
    }
}

impl Clone for ConfirmOverlay {
    /// A fresh dialog holding the same command; ratcn's retained surface is
    /// per instance and is rebuilt on the clone's first frame.
    fn clone(&self) -> Self {
        let mut out = Self::new(
            self.title,
            self.lines.clone(),
            self.buttons,
            self.on_yes.clone(),
            &self.theme,
        );
        out.breakpoints = self.breakpoints;
        out
    }
}

impl ConfirmOverlay {
    fn new(
        title: &'static str,
        lines: Vec<String>,
        buttons: Buttons,
        on_yes: OverlayCommand,
        theme: &Theme,
    ) -> Self {
        let runtime = Ratcn::new().focus(|m: &Model| &m.focus, Msg::Focus);
        Self {
            title,
            lines,
            buttons,
            on_yes,
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
            // Focus starts on the confirming action, so Enter commits as it
            // always has.
            model: Model {
                focus: FocusState::intent([DIALOG, YES]),
            },
            ui: Surface::new(runtime),
            outer: Cell::new(Rect::new(0, 0, 80, 24)),
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
        let buttons = Buttons {
            yes: "Paste",
            no: "Cancel",
            destructive: false,
        };
        Self::new("Paste?", vec![message], buttons, on_yes, theme)
    }

    /// Ask before `source` (a pane label) sets the clipboard to `text`.
    #[must_use]
    pub fn clipboard(source: &str, text: ClipboardText, theme: &Theme) -> Self {
        let message = format!(
            "{source} wants to set the clipboard ({} bytes)",
            text.0.len()
        );
        let on_yes = OverlayCommand::SetClipboard(text);
        let buttons = Buttons {
            yes: "Allow",
            no: "Deny",
            destructive: false,
        };
        Self::new("Clipboard?", vec![message], buttons, on_yes, theme)
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
        let on_yes = OverlayCommand::Commit(crate::attach::resolved(
            action,
            BTreeMap::from([(KILL_TARGETS_ARG.to_owned(), toml::Value::Array(selectors))]),
        ));
        let buttons = Buttons {
            yes: "Kill",
            no: "Cancel",
            destructive: true,
        };
        Self::new(title, lines, buttons, on_yes, theme)
    }

    /// The lines the modal shows, top to bottom.
    #[cfg(test)]
    pub(crate) fn lines(&self) -> &[String] {
        &self.lines
    }

    fn modal_area(&self, outer: Rect) -> Rect {
        dialog_area(outer, &self.lines.join("\n"), self.breakpoints)
    }

    /// Declare the dialog filling `area` (the modal rect).
    fn declare(&self, area: Rect) -> impl FnMut(&mut DeclareCtx<'_, Model, Msg>) + use<> {
        let title = self.title;
        let description = self.lines.join("\n");
        let Buttons {
            yes,
            no,
            destructive,
        } = self.buttons;
        move |ctx| {
            let confirm = Button::new(yes).on_press(|| Msg::Yes);
            let confirm = if destructive {
                confirm.destructive()
            } else {
                confirm
            };
            let dialog = Dialog::new()
                .title(title)
                .description(description.clone())
                .outer_width(area.width)
                .outer_height(area.height)
                .on_dismiss(|| Msg::No)
                .action(NO, Button::new(no).secondary().on_press(|| Msg::No))
                .action(YES, confirm);
            ctx.component(DIALOG, dialog, area);
        }
    }

    /// Route one ratcn event and fold its message into a command.
    fn route(&mut self, event: Event) -> OverlayCommand {
        let area = self.modal_area(self.outer.get());
        let declare = self.declare(area);
        let theme = kit::theme(&self.theme);
        match self.ui.route(event, &self.model, &theme, declare) {
            EventResult::Emit(Msg::Focus(focus)) => {
                self.model.focus = focus;
                OverlayCommand::Stay
            }
            EventResult::Emit(Msg::Yes) => self.on_yes.clone(),
            EventResult::Emit(Msg::No) => OverlayCommand::Dismiss,
            EventResult::Consumed | EventResult::Ignored => OverlayCommand::Stay,
        }
    }
}

/// Rows a ratcn dialog adds around its description: border and padding
/// above and below, the gap row, and the one-row action buttons.
const DIALOG_CHROME_ROWS: u16 = 2 + 2 + 1 + 1;

/// A half-width ratcn dialog exactly tall enough for `description` wrapped
/// to its inner width, centered.
pub(crate) fn dialog_area(outer: Rect, description: &str, bp: ChromeBreakpoints) -> Rect {
    let wide = centered_panel(outer, 5, 40, DIALOG_CHROME_ROWS + 1, bp);
    let inner = wide.width.saturating_sub(4);
    let rows =
        ratcn::geometry::wrapped_height(description, inner).saturating_add(DIALOG_CHROME_ROWS);
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
        self.outer.set(area);
        let modal = self.modal_area(area);
        let theme = kit::theme(&self.theme);
        self.ui
            .paint(buf, modal, &self.model, &theme, self.declare(modal));
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(self.modal_area(area))
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
        self.ui.invalidate();
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = *theme;
    }

    fn on_viewport_resize(&mut self, _pane_cols: u16, _pane_rows: u16) {
        self.ui.invalidate();
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        // The letter shortcuts answer whichever button has focus.
        match key.key {
            PhysicalKey::Y => return self.on_yes.clone(),
            PhysicalKey::N => return OverlayCommand::Dismiss,
            // Only Enter or `y` confirms, as before: a stray Space typed
            // into a held paste must not press the focused button.
            PhysicalKey::Space => return OverlayCommand::Stay,
            _ => {}
        }
        kit::key(key).map_or(OverlayCommand::Stay, |event| self.route(event))
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        kit::mouse(mouse).map_or(OverlayCommand::Stay, |event| self.route(event))
    }
}

#[cfg(test)]
mod tests {
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::input::paste::PasteTrust;

    use phux_protocol::input::key::ModSet;
    use phux_protocol::input::mouse::{MouseAction, MouseButton};

    use super::*;
    use crate::test_util::{press, rendered};

    #[test]
    fn the_dialog_paints_its_title_message_and_both_buttons() {
        let out = rendered(&clipboard(), 80, 24);
        assert!(out.contains("Clipboard?"), "{out}");
        assert!(out.contains("pane 3 wants to set the clipboard"), "{out}");
        assert!(out.contains("Allow") && out.contains("Deny"), "{out}");
    }

    #[test]
    fn space_never_confirms_a_held_paste() {
        let mut overlay = ConfirmOverlay::paste(paste(b"rm -rf ~\n"), 1, false, &Theme::default());
        let space = crate::test_util::press_text(PhysicalKey::Space, Some(" "));
        assert_eq!(overlay.handle_key(&space), OverlayCommand::Stay);
    }

    #[test]
    fn tab_moves_focus_to_the_declining_button_and_enter_presses_it() {
        let mut overlay = clipboard();
        let area = Rect::new(0, 0, 80, 24);
        overlay.render(area, &mut Buffer::empty(area));
        let back = crate::test_util::key_event(PhysicalKey::Tab, ModSet::SHIFT, None);
        assert_eq!(overlay.handle_key(&back), OverlayCommand::Stay);
        assert_eq!(
            overlay.handle_key(&press(PhysicalKey::Enter)),
            OverlayCommand::Dismiss
        );
    }

    #[test]
    fn clicking_a_button_presses_it() {
        let mut overlay = clipboard();
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        overlay.render(area, &mut buf);
        let (x, y) = (0..area.height)
            .find_map(|y| {
                let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
                row.find("Allow").map(|x| (x, y))
            })
            .expect("the Allow button is painted");
        let at = |action| MouseEvent {
            action,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x: f64::from(u16::try_from(x).expect("fits") + 1),
            y: f64::from(y),
        };
        assert_eq!(
            overlay.handle_mouse(&at(MouseAction::Press)),
            OverlayCommand::Stay
        );
        assert_eq!(
            overlay.handle_mouse(&at(MouseAction::Release)),
            OverlayCommand::SetClipboard(ClipboardText("secret".to_owned()))
        );
    }

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
