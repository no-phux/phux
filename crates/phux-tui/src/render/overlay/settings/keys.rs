//! The keyboard and pointer entry points, and the overlay trait.

use super::{
    Buffer, ChromeBreakpoints, EDITOR_KEYS, Event, KeyAction, KeyEvent, Layout, Line, ModSet,
    Modal, MouseAction, MouseButton, MouseEvent, OverlayCommand, PhysicalKey, Rect, RenderOverlay,
    SettingsOverlay, Span, Style, Theme, WHEEL_SCROLL_ROWS, kit, settings_legend,
};

impl SettingsOverlay {
    /// The `C-` chords: `C-n`/`C-p` move, `C-r` resets, `C-z` undoes, `C-u`
    /// clears the query. `None` for a chord this page does not bind.
    pub(super) fn handle_ctrl_key(
        &mut self,
        key: PhysicalKey,
        len: usize,
    ) -> Option<OverlayCommand> {
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
    pub(super) fn navigate(&mut self, key: PhysicalKey, len: usize) -> bool {
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

impl SettingsOverlay {
    /// The browsing keys; [`RenderOverlay::handle_key`] settles focus after.
    pub(super) fn handle_browse_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        let len = self.visible_specs().len();
        self.clamp_selection(len);
        if key.mods.contains(ModSet::CTRL)
            && let Some(command) = self.handle_ctrl_key(key.key, len)
        {
            return command;
        }
        match key.key {
            PhysicalKey::Escape => self.clear_query_or_dismiss(),
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
                self.step_section(key.mods.contains(ModSet::SHIFT));
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
                self.type_filter_text(key.text.as_deref());
                OverlayCommand::Stay
            }
        }
    }

    /// The pointer: ratcn first (tabs, boxes, cycles, rows, the viewport's
    /// wheel and scrollbar); a wheel it leaves (over the tabs or detail, or
    /// at the end of the rows) moves the selection.
    pub(super) fn handle_pointer(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        let Some(event) = kit::mouse(mouse) else {
            return OverlayCommand::Stay;
        };
        let (command, handled) = self.route(event);
        if handled || mouse.action != MouseAction::Press || self.editor.is_some() {
            return command;
        }
        let len = self.visible_specs().len();
        match mouse.button {
            MouseButton::Four => (0..WHEEL_SCROLL_ROWS).for_each(|_| self.select_up()),
            MouseButton::Five => (0..WHEEL_SCROLL_ROWS).for_each(|_| self.select_down(len)),
            _ => {}
        }
        command
    }
}

impl RenderOverlay for SettingsOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.outer.set(area);
        let layout = self.settle(area);
        let Layout {
            modal,
            width,
            detail,
            ..
        } = layout;
        let specs = self.visible_specs();

        // The frame, the query line, and the detail panel are the page's own
        // paint; the tab row and the rows are blank here for ratcn.
        let mut lines: Vec<Line<'static>> = vec![self.header_row(width)];
        lines.resize(1 + 1 + layout.list_height(), Line::from(""));
        lines.push(Line::from(Span::styled(
            "\u{2500}".repeat(width),
            Style::default().fg(self.theme.border),
        )));
        let mut detail_lines = self.detail_lines(specs.get(self.selected).copied(), width, detail);
        detail_lines.resize(usize::from(detail), Line::from(""));
        lines.extend(detail_lines);

        let status = self.footer_status(specs.len());
        let keys = if self.editor.is_some() {
            EDITOR_KEYS
        } else {
            settings_legend(modal.width, &status)
        };
        Modal::new(&self.theme, "Settings", lines)
            .footer(status, keys)
            .render_into(modal, buf);

        let theme = kit::theme(&self.theme);
        self.ui.paint(
            buf,
            layout.surface(),
            &self.model,
            &theme,
            self.declare(&layout),
        );
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
        if self.editor.is_some() {
            self.route(Event::Paste(text.to_owned()));
        } else {
            self.query.push_str(text);
            self.requery();
        }
        self.sync_focus();
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        let command = self.handle_pointer(mouse);
        self.sync_focus();
        command
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        let command = if self.editor.is_some() {
            self.handle_editor_key(key)
        } else {
            self.handle_browse_key(key)
        };
        self.sync_focus();
        command
    }
}
