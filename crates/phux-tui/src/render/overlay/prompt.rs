//! Prompt overlay: a ratcn [`Input`] in a [`Dialog`], committing
//! `action { arg_key: <text> }`.
//!
//! Enter in the field (or on the verb button) commits; an empty field
//! cancels instead of committing a blank name. Esc, `Cancel`, and an empty
//! commit dismiss. Tab moves between the field and the buttons, and the
//! mouse clicks either.
//!
//! The field keeps the readline set (`C-a`/`C-e`/`C-b`/`C-f`, `C-h`/`C-d`,
//! `C-k`, `C-w`), and the two edits ratcn's editor leaves out are made here:
//! `C-u` kills to the start, and Ctrl+Backspace (the legacy BS byte) is a
//! Backspace. Pastes and multi-character key text insert literally.

use std::cell::Cell;
use std::collections::BTreeMap;

use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::MouseEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratcn::runtime::{DeclareCtx, Event, EventResult, FocusState, Ratcn};
use ratcn::text_edit::Editor;
use ratcn::{Button, Dialog, Input, InputState};

use super::widgets::centered_panel;
use super::{OverlayCommand, RenderOverlay};
use crate::render::kit::{self, Surface};
use crate::render::{ChromeBreakpoints, Theme};

const DIALOG: &str = "prompt";
const INPUT: &str = "input";
const CANCEL: &str = "cancel";
const COMMIT: &str = "commit";

/// Rows a one-field ratcn dialog takes: border and padding above and
/// below, the field, the gap row, and the button row.
const PROMPT_ROWS: u16 = 2 + 2 + 1 + 1 + 1;

/// What the prompt's ratcn surface tracks between frames.
#[derive(Debug)]
struct Model {
    focus: FocusState,
    input: InputState,
}

/// What the field, the buttons, and focus traversal ask for.
#[derive(Debug, Clone)]
enum Msg {
    Focus(FocusState),
    Input(InputState),
    Submit,
    Cancel,
}

/// A single-line text-input modal that commits to an action.
#[derive(Debug)]
pub struct PromptOverlay {
    /// Modal title (e.g. `"Rename tab"`).
    title: String,
    /// The action name to run on commit (e.g. `"rename-window"`).
    action: String,
    /// The committing button's label, naming what Enter does (`Rename`).
    verb: &'static str,
    /// Dim text shown while the input is empty, saying what it wants.
    placeholder: &'static str,
    /// The arg key the typed text is bound to (e.g. `"name"`).
    arg_key: String,
    /// Color slots snapshotted from the active [`Theme`].
    theme: Theme,
    /// `[chrome]` thresholds, stamped by `OverlayState::push`.
    breakpoints: ChromeBreakpoints,
    model: Model,
    ui: Surface<Model, Msg>,
    /// The viewport of the last paint, which input is laid out against.
    outer: Cell<Rect>,
}

impl PromptOverlay {
    /// A prompt committing `action { arg_key: <text> }`, pre-filled with
    /// `initial`.
    #[must_use]
    pub fn new(title: &str, action: &str, arg_key: &str, initial: &str, theme: &Theme) -> Self {
        let runtime = Ratcn::new().focus(|m: &Model| &m.focus, Msg::Focus);
        Self {
            title: title.to_owned(),
            action: action.to_owned(),
            verb: "OK",
            placeholder: "",
            arg_key: arg_key.to_owned(),
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
            model: Model {
                focus: FocusState::intent([DIALOG, INPUT]),
                input: InputState::new(initial),
            },
            ui: Surface::new(runtime),
            outer: Cell::new(Rect::new(0, 0, 80, 24)),
        }
    }

    /// The `rename-window` prompt, pre-filled with the window's current
    /// name (empty for an auto-named tab) and styled with `theme`.
    #[must_use]
    pub fn rename_window(current_name: &str, theme: &Theme) -> Self {
        Self {
            verb: "Rename",
            placeholder: "a name for it",
            ..Self::new("Rename tab", "rename-window", "name", current_name, theme)
        }
    }

    /// The `rename-session` prompt, pre-filled with the current name.
    #[must_use]
    pub fn rename_session(current_name: &str, theme: &Theme) -> Self {
        Self {
            verb: "Rename",
            ..Self::new(
                "Rename session",
                "rename-session",
                "name",
                current_name,
                theme,
            )
        }
    }

    /// The `new-session` prompt; committing re-attaches to the new session.
    #[must_use]
    pub fn new_session(theme: &Theme) -> Self {
        Self {
            verb: "Create",
            placeholder: "a name for it",
            ..Self::new("New session", "new-session", "name", "", theme)
        }
    }

    /// The `popup` prompt; committing runs the command in a floating pane.
    #[must_use]
    pub fn popup(theme: &Theme) -> Self {
        Self {
            verb: "Run",
            placeholder: "a command",
            ..Self::new("Popup", "popup", "command", "", theme)
        }
    }

    /// The `new-worktree` prompt; committing creates the worktree and
    /// switches to its session.
    #[must_use]
    pub fn new_worktree(theme: &Theme) -> Self {
        Self {
            verb: "Create",
            placeholder: "a branch name",
            ..Self::new("New worktree", "new-worktree", "branch", "", theme)
        }
    }

    /// The commit for the current text, or a dismissal when it is blank.
    fn submit(&self) -> OverlayCommand {
        let text = self.model.input.value();
        if text.trim().is_empty() {
            return OverlayCommand::Dismiss;
        }
        let args = BTreeMap::from([(self.arg_key.clone(), toml::Value::String(text.to_owned()))]);
        OverlayCommand::Commit(crate::attach::resolved(&self.action, args))
    }

    fn modal_area(&self, outer: Rect) -> Rect {
        let wide = centered_panel(outer, 5, 40, PROMPT_ROWS, self.breakpoints);
        let h = PROMPT_ROWS.min(outer.height);
        let y = outer.y + (outer.height.saturating_sub(h)) / 2;
        Rect::new(wide.x, y, wide.width, h)
    }

    /// Declare the dialog filling `area` (the modal rect).
    fn declare(&self, area: Rect) -> impl FnMut(&mut DeclareCtx<'_, Model, Msg>) + use<> {
        let title = self.title.clone();
        let placeholder = self.placeholder;
        let verb = self.verb;
        move |ctx| {
            let dialog = Dialog::new()
                .title(title.clone())
                .outer_width(area.width)
                .outer_height(area.height)
                .on_dismiss(|| Msg::Cancel)
                .content(1, move |ctx| {
                    let field = Input::new()
                        .value(|m: &Model| &m.input, Msg::Input)
                        .placeholder(placeholder)
                        .on_submit(|| Msg::Submit);
                    let row = ctx.area();
                    ctx.component(INPUT, field, Rect::new(row.x, row.y, row.width, 1));
                })
                .action(
                    CANCEL,
                    Button::new("Cancel").secondary().on_press(|| Msg::Cancel),
                )
                .action(COMMIT, Button::new(verb).on_press(|| Msg::Submit));
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
            EventResult::Emit(Msg::Input(input)) => {
                self.model.input = input;
                OverlayCommand::Stay
            }
            EventResult::Emit(Msg::Submit) => self.submit(),
            EventResult::Emit(Msg::Cancel) => OverlayCommand::Dismiss,
            EventResult::Consumed | EventResult::Ignored => OverlayCommand::Stay,
        }
    }
}

/// `text` inserted at the field's cursor exactly as given, minus control
/// characters (a paste, or key text ratcn has no single key for).
pub(super) fn insert_literal(input: &InputState, text: &str) -> InputState {
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    edited(input, |editor| {
        editor.insert_str(clean);
    })
}

/// `input` after `edit` is applied to a copy of its editor.
fn edited(input: &InputState, edit: impl FnOnce(&mut Editor<'static>)) -> InputState {
    let mut editor = input.editor().clone();
    edit(&mut editor);
    InputState::from_editor(editor)
}

/// A key for a focused text field: either applied to `input` here (the
/// readline edits ratcn's editor leaves out, and key text ratcn has no
/// single key for), or the ratcn event to route.
pub(super) fn field_key(input: &mut InputState, key: &KeyEvent) -> Option<Event> {
    let ctrl = key.mods.contains(ModSet::CTRL);
    if ctrl && key.key == PhysicalKey::U {
        *input = edited(input, |editor| {
            editor.delete_line_by_head();
        });
        return None;
    }
    if ctrl && matches!(key.key, PhysicalKey::Backspace) {
        // BS (0x08) decodes as Ctrl+Backspace; it is a plain Backspace here.
        let plain = KeyEvent {
            mods: key.mods - ModSet::CTRL,
            ..key.clone()
        };
        return kit::key(&plain);
    }
    let event = kit::key(key);
    if event.is_none()
        && !key.mods.intersects(ModSet::CTRL | ModSet::ALT)
        && let Some(text) = key.text.as_deref()
        && !text.is_empty()
        && !text.chars().any(char::is_control)
    {
        *input = insert_literal(input, text);
    }
    event
}

impl RenderOverlay for PromptOverlay {
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

    fn handle_paste(&mut self, text: &str) {
        self.model.input = insert_literal(&self.model.input, text);
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        // Press-only; ignore release/repeat so a held key doesn't double.
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        let on_field = self.model.focus.contains_path([DIALOG, INPUT]);
        let event = if on_field {
            field_key(&mut self.model.input, key)
        } else {
            kit::key(key)
        };
        event.map_or(OverlayCommand::Stay, |event| self.route(event))
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        kit::mouse(mouse).map_or(OverlayCommand::Stay, |event| self.route(event))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
