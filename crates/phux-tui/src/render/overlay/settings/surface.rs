//! The ratcn surface: section tabs, the row viewport, and the value
//! controls, and the messages they send back.

use super::{
    Alignment, Buffer, Cell, Checkbox, CheckboxStyle, ChromeBreakpoints, Component, Cycle,
    DeclareCtx, Event, EventResult, FIXED_ROWS, FocusState, Input, InputState, Line, MODAL_PAD,
    OverlayCommand, PaintCtx, Rect, ScopeOptions, ScrollArea, ScrollAreaStyle, SettingKind,
    SettingSection, SettingSpec, SettingsOverlay, Style, TOGGLE_COLS, TRI_STATE, Tab, Tabs,
    TabsActivation, display_width, kit, modal_inner_width, scroll_into_view,
};

/// Identities on the ratcn surface: the section tabs, the row viewport, and
/// the value control inside a row (rows are keyed by their setting key).
pub(super) const TABS: &str = "tabs";
pub(super) const ROWS: &str = "rows";
pub(super) const VALUE: &str = "value";

/// What the ratcn surface reads: focus (always the selected row's control,
/// or the editor), the editor's text, and the row viewport's offset (a
/// `Cell`: resolved at paint, which takes `&self`).
#[derive(Default)]
pub(super) struct Model {
    pub(super) focus: FocusState,
    pub(super) input: InputState,
    pub(super) scroll: Cell<usize>,
}

/// What the tabs, rows, and controls ask for.
pub(super) enum Msg {
    /// A press focused something; a row's focus selects that row.
    Focus(FocusState),
    /// A tab was clicked: show that section.
    Section(usize),
    /// The viewport scrolled (wheel, scrollbar) to this first row.
    Scroll(u16),
    /// A checkbox was clicked.
    Toggle(&'static str),
    /// A cycle was clicked: step forward (`1`) or back (`-1`).
    Step(&'static str, i64),
    /// The value editor's text or cursor changed.
    Edit(InputState),
}

/// The control a row's value cell shows, captured at declaration.
#[derive(Debug, Clone)]
pub(super) enum Control {
    Toggle(bool),
    Cycle {
        options: Vec<&'static str>,
        at: usize,
    },
    Editor,
}

impl Control {
    /// The columns the control's slot holds, sized by its widest state so
    /// stepping or toggling never moves the row (`None` for the editor,
    /// which takes what the key leaves).
    pub(super) fn slot(&self) -> Option<usize> {
        match self {
            Self::Toggle(_) => Some(TOGGLE_COLS),
            Self::Cycle { options, .. } => {
                Some(options.iter().map(|o| display_width(o)).max().unwrap_or(0) + 2)
            }
            Self::Editor => None,
        }
    }
}

/// One setting row as declared: its painted line, and the control that sits
/// over its value cell at `(x offset, width)`.
#[derive(Debug, Clone)]
pub(super) struct RowDecl {
    pub(super) key: &'static str,
    pub(super) line: Line<'static>,
    pub(super) control: Option<(Control, u16, u16)>,
}

/// Where the modal's parts sit for one viewport.
#[derive(Debug, Clone, Copy)]
pub(super) struct Layout {
    pub(super) modal: Rect,
    /// Interior text width.
    pub(super) width: usize,
    pub(super) detail: u16,
    /// The section tab row.
    pub(super) tabs: Rect,
    /// The row viewport, its scrollbar gutter riding the right border.
    pub(super) rows: Rect,
}

impl Layout {
    /// The rows the list viewport shows.
    pub(super) const fn list_height(&self) -> usize {
        self.rows.height as usize
    }

    /// Everything the ratcn surface paints: the tabs and the rows.
    pub(super) const fn surface(&self) -> Rect {
        Rect::new(
            self.tabs.x,
            self.tabs.y,
            self.rows.width,
            self.rows.height + 1,
        )
    }
}

/// A settings row: paints its line and holds its value control.
pub(super) struct Row {
    pub(super) decl: RowDecl,
}

impl Component<Model, Msg> for Row {
    fn declare(&mut self, ctx: &mut DeclareCtx<'_, Model, Msg>) {
        let Some((control, dx, w)) = self.decl.control.take() else {
            return;
        };
        let area = ctx.area();
        let rect = Rect::new(area.x.saturating_add(dx), area.y, w, 1);
        let key = self.decl.key;
        match control {
            Control::Toggle(checked) => {
                let label = if checked { "true" } else { "false" };
                let checkbox = Checkbox::new(label)
                    .checked(move |_| checked, move |_| Msg::Toggle(key))
                    .style(|t| CheckboxStyle {
                        checked_marker_color: t.accent,
                        ..CheckboxStyle::from_theme(t)
                    });
                ctx.component(VALUE, checkbox, rect);
            }
            Control::Cycle { options, at } => {
                let len = options.len();
                let cycle = Cycle::new(options).align(Alignment::Right).selection(
                    move |_| at,
                    move |next| Msg::Step(key, step_of(at, next, len)),
                );
                ctx.component(VALUE, cycle, rect);
            }
            Control::Editor => {
                let input = Input::new().value(|m: &Model| &m.input, Msg::Edit);
                ctx.component(VALUE, input, rect);
            }
        }
    }

    fn paint(&mut self, ctx: &mut PaintCtx<'_, Model>) {
        ctx.widget(self.decl.line.clone(), ctx.area());
    }

    fn scope_options(&self, _state: &Model) -> ScopeOptions {
        ScopeOptions::default().focusable(true)
    }
}

/// The direction a cycle moved from `at` to `next` among `len` options.
const fn step_of(at: usize, next: usize, len: usize) -> i64 {
    if len > 0 && next == (at + 1) % len {
        1
    } else {
        -1
    }
}

impl SettingsOverlay {
    /// Where everything sits over `outer`: the tab row under the query
    /// line, the rows under it down to the detail rule, the row viewport's
    /// scrollbar on the right border.
    pub(super) fn layout(outer: Rect, bp: ChromeBreakpoints) -> Layout {
        let modal = Self::modal_area(outer, bp);
        let inner = modal_inner_width(modal.width);
        let detail = Self::detail_rows(outer, bp);
        let x = modal.x + 1 + MODAL_PAD;
        let list_height = modal.height.saturating_sub(FIXED_ROWS + detail);
        Layout {
            modal,
            width: usize::from(inner),
            detail,
            tabs: Rect::new(x, modal.y + 2, inner, 1),
            // The interior, the right pad, and the border column the
            // scrollbar gutter takes.
            rows: Rect::new(x, modal.y + 3, inner + MODAL_PAD + 1, list_height),
        }
    }

    /// The layout over `outer`, with the scroll offset settled so the
    /// selection is in view.
    pub(super) fn settle(&self, outer: Rect) -> Layout {
        let layout = Self::layout(outer, self.breakpoints);
        let rows = layout.list_height();
        let len = self.visible_specs().len();
        let offset = scroll_into_view(self.model.scroll.get(), self.selected, len, rows);
        self.page.set(rows);
        self.model.scroll.set(offset);
        layout
    }

    /// The control `spec`'s value cell shows: the editor while it is open
    /// on this row, a checkbox for a bool, a cycle for a choice or a
    /// tri-state; `None` paints the value as text.
    pub(super) fn control_for(&self, spec: &SettingSpec) -> Option<Control> {
        if self.editor == Some(spec.key) {
            return Some(Control::Editor);
        }
        let value = self.value_at(spec.key);
        match spec.kind {
            SettingKind::Bool => value.and_then(|v| v.as_bool()).map(Control::Toggle),
            SettingKind::Choice(variants) => {
                let current = value.as_ref().and_then(toml::Value::as_str)?;
                let at = variants.iter().position(|v| *v == current)?;
                Some(Control::Cycle {
                    options: variants.to_vec(),
                    at,
                })
            }
            SettingKind::OptionalBool => {
                let at = match value.and_then(|v| v.as_bool()) {
                    None => 0,
                    Some(true) => 1,
                    Some(false) => 2,
                };
                Some(Control::Cycle {
                    options: TRI_STATE.to_vec(),
                    at,
                })
            }
            _ => None,
        }
    }

    /// The section tabs: each title with a dot cell held on every tab and
    /// filled when the section holds overrides, so an edit never reflows
    /// the row; `true` dims a section that holds no match while filtering.
    pub(super) fn tab_labels(&self) -> Vec<(String, bool)> {
        SettingSection::ALL
            .iter()
            .map(|section| {
                let dot = if self.overridden_in(*section) > 0 {
                    "\u{2022}"
                } else {
                    " "
                };
                let empty = !self.query.is_empty() && self.matches_in(*section) == 0;
                (format!("{}{dot}", section.title()), empty)
            })
            .collect()
    }

    /// The ratcn surface over `layout`: the section tabs, then every
    /// visible row in a viewport scrolled to the settled offset.
    pub(super) fn declare(
        &self,
        layout: &Layout,
    ) -> impl FnMut(&mut DeclareCtx<'_, Model, Msg>) + use<> {
        let specs = self.visible_specs();
        let full_key = !self.query.is_empty();
        let rows: Vec<RowDecl> = specs
            .iter()
            .enumerate()
            .map(|(i, spec)| self.row_decl(spec, i == self.selected, layout.width, full_key))
            .collect();
        let tabs = self.tab_labels();
        let active = self.query.is_empty().then_some(self.section);
        let (tabs_rect, rows_rect) = (layout.tabs, layout.rows);
        let row_w = u16::try_from(layout.width).unwrap_or(u16::MAX);
        let dim = Style::default().fg(self.theme.dim);
        move |ctx| {
            let items = tabs
                .iter()
                .enumerate()
                .map(|(i, (label, empty))| Tab::new(i, label.clone()).disabled(*empty));
            let tab_row = Tabs::new(items)
                .activation(TabsActivation::Automatic)
                .selection(move |_| active, Msg::Section);
            ctx.component(TABS, tab_row, tabs_rect);
            if rows.is_empty() {
                ctx.paint_widget(Line::styled("(no matches)", dim), rows_rect);
                return;
            }
            let rows = rows.clone();
            let height = u16::try_from(rows.len()).unwrap_or(u16::MAX);
            let area = ScrollArea::new(height)
                .scroll(
                    |m: &Model| u16::try_from(m.scroll.get()).unwrap_or(u16::MAX),
                    Msg::Scroll,
                )
                .style(|t| ScrollAreaStyle {
                    thumb: t.muted_foreground,
                    track: t.border,
                })
                .content(move |ctx| {
                    let top = ctx.area();
                    for (y, decl) in (top.y..).zip(rows) {
                        let rect = Rect::new(top.x, y, row_w, 1);
                        ctx.component(decl.key, Row { decl }, rect);
                    }
                });
            ctx.component(ROWS, area, rows_rect);
        }
    }

    /// Point ratcn's focus at the selection: the editor while it is open,
    /// else the selected row's control, else the row itself.
    pub(super) fn sync_focus(&mut self) {
        let len = self.visible_specs().len();
        self.clamp_selection(len);
        self.model.focus = match (self.editor, self.selected_spec()) {
            (Some(key), _) => FocusState::intent([ROWS, key, VALUE]),
            (None, Some(spec)) if self.control_for(spec).is_some() => {
                FocusState::intent([ROWS, spec.key, VALUE])
            }
            (None, Some(spec)) => FocusState::intent([ROWS, spec.key]),
            (None, None) => FocusState::none(),
        };
    }

    /// Route one ratcn event against the surface the current state
    /// declares and fold its message in; the flag says whether ratcn
    /// handled the event at all.
    pub(super) fn route(&mut self, event: Event) -> (OverlayCommand, bool) {
        let outer = self.outer.get();
        let layout = self.settle(outer);
        let theme = kit::theme(&self.theme);
        // Re-declare first: an edit, a section switch, or the editor opening
        // since the last frame changes what the surface holds.
        let mut scratch = Buffer::empty(outer);
        self.ui.paint(
            &mut scratch,
            layout.surface(),
            &self.model,
            &theme,
            self.declare(&layout),
        );
        let declare = self.declare(&layout);
        match self.ui.route(event, &self.model, &theme, declare) {
            EventResult::Emit(msg) => (self.update(msg), true),
            EventResult::Consumed => (OverlayCommand::Stay, true),
            EventResult::Ignored => (OverlayCommand::Stay, false),
        }
    }

    /// Fold one surface message into the page. While the editor is open
    /// only its own edits land: the list holds still under it.
    pub(super) fn update(&mut self, msg: Msg) -> OverlayCommand {
        if self.editor.is_some() {
            if let Msg::Edit(input) = msg {
                self.model.input = input;
            }
            return OverlayCommand::Stay;
        }
        match msg {
            Msg::Focus(focus) => {
                if let Some(index) = self.row_of(&focus) {
                    self.selected = index;
                }
                OverlayCommand::Stay
            }
            Msg::Section(index) => {
                self.select_section(index);
                OverlayCommand::Stay
            }
            Msg::Scroll(offset) => {
                self.follow_scroll(usize::from(offset));
                OverlayCommand::Stay
            }
            Msg::Toggle(key) => self
                .spec(key)
                .map_or(OverlayCommand::Stay, |spec| self.activate(spec)),
            Msg::Step(key, direction) => self
                .spec(key)
                .map_or(OverlayCommand::Stay, |spec| self.cycle(spec, direction)),
            Msg::Edit(_) => OverlayCommand::Stay,
        }
    }

    /// The setting at `key`.
    pub(super) fn spec(&self, key: &str) -> Option<&'static SettingSpec> {
        self.specs.iter().copied().find(|s| s.key == key)
    }

    /// The visible row a focus path names.
    pub(super) fn row_of(&self, focus: &FocusState) -> Option<usize> {
        let path = focus.path();
        if *path.first()? != ROWS {
            return None;
        }
        let key = path.get(1)?;
        self.visible_specs().iter().position(|s| *key == s.key)
    }

    /// The viewport scrolled to `offset`: keep the selection inside it.
    pub(super) fn follow_scroll(&mut self, offset: usize) {
        self.model.scroll.set(offset);
        let last = offset + self.page_rows() - 1;
        self.selected = self.selected.clamp(offset, last);
        let len = self.visible_specs().len();
        self.clamp_selection(len);
    }
}
