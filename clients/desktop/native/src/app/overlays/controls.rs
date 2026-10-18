//! Modal preparation, text input, and confirmed palette actions.

use super::*;

/// Build the open modal's controls with fresh or prefilled values and give it the keyboard.
pub(crate) fn prepare(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) {
    match desk.modal.clone() {
        Modal::None => {}
        Modal::Capture => desk.prepare_capture(window, cx),
        Modal::Commands
        | Modal::Goto
        | Modal::WindowSession
        | Modal::Windows
        | Modal::MoveResources
        | Modal::Path => {
            desk.palette_view = PaletteView::default();
            let state = palette_state(desk, window, cx);
            state.update(cx, |state, cx| {
                state.set_loading(false, window, cx);
                state.set_query("", window, cx);
                state.focus(window, cx);
            });
        }
        Modal::Settings => {
            desk.settings_page = None;
            let state = palette_state(desk, window, cx);
            state.update(cx, |state, cx| {
                state.set_query("", window, cx);
                state.focus(window, cx);
            });
        }
        Modal::Rename { tab_id } => {
            let current = desk
                .link
                .as_ref()
                .and_then(|link| link.workspace.tabs.iter().find(|tab| tab.id == tab_id))
                .map(|tab| desk::tab_title(desk, tab))
                .unwrap_or_default();
            let input = modal_input(
                desk,
                InputSlot::Rename,
                &current,
                "Follow the terminal title",
                window,
                cx,
            );
            input.update(cx, |input, cx| input.focus(window, cx));
        }
        Modal::Session {
            create, current, ..
        } => {
            let name = modal_input(
                desk,
                InputSlot::SessionName,
                if create { "" } else { &current },
                "Session name",
                window,
                cx,
            );
            if create {
                let directory = desk.focused_cwd().unwrap_or_default();
                modal_input(
                    desk,
                    InputSlot::SessionDir,
                    &directory,
                    "Directory",
                    window,
                    cx,
                );
            }
            name.update(cx, |input, cx| input.focus(window, cx));
        }
        Modal::AddHost => {
            let target = modal_input(
                desk,
                InputSlot::HostTarget,
                "",
                "ssh destination, e.g. me@mini",
                window,
                cx,
            );
            modal_input(
                desk,
                InputSlot::HostName,
                "",
                "Name (optional, defaults to the host)",
                window,
                cx,
            );
            target.update(cx, |input, cx| input.focus(window, cx));
        }
        Modal::KillSession { .. }
        | Modal::NativeLaunch
        | Modal::Terminate { .. }
        | Modal::RemoveHost { .. }
        | Modal::Paste { .. }
        | Modal::Notifications => window.focus(&desk.focus, cx),
    }
}

fn palette_state(
    desk: &mut Desk,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> Entity<CommandState> {
    if let Some(state) = desk.palette_state.clone() {
        return state;
    }
    let state = cx.new(|cx| CommandState::new(window, cx));
    desk.palette_state = Some(state.clone());
    state
}

fn modal_input(
    desk: &mut Desk,
    slot: InputSlot,
    initial: &str,
    placeholder: &str,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> Entity<InputState> {
    let (input, subscription) = input_with(slot, initial, placeholder, window, cx);
    desk.modal_inputs.push((slot, input.clone(), subscription));
    input
}

/// The find field, focused, reporting each edit and Enter / Shift-Enter.
pub(crate) fn find_input(
    initial: &str,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> (Entity<InputState>, Subscription) {
    let (input, subscription) =
        input_with(InputSlot::Find, initial, "Find in terminal", window, cx);
    input.update(cx, |input, cx| input.focus(window, cx));
    (input, subscription)
}

fn input_with(
    slot: InputSlot,
    initial: &str,
    placeholder: &str,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> (Entity<InputState>, Subscription) {
    let initial = initial.to_owned();
    let placeholder = placeholder.to_owned();
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .default_value(initial)
    });
    let subscription = cx.subscribe_in(
        &input,
        window,
        move |desk, state, event: &InputEvent, window, cx| {
            let value = state.read(cx).value().to_string();
            match event {
                InputEvent::Change => desk.on_input(slot, value, None, window, cx),
                InputEvent::PressEnter { shift, .. } => {
                    desk.on_input(slot, value, Some(*shift), window, cx)
                }
                _ => {}
            }
        },
    );
    (input, subscription)
}

impl Desk {
    pub(super) fn input_entity(&self, slot: InputSlot) -> Option<Entity<InputState>> {
        self.modal_inputs
            .iter()
            .find(|(owned, _, _)| *owned == slot)
            .map(|(_, input, _)| input.clone())
    }

    pub(super) fn input_value(&self, slot: InputSlot, cx: &App) -> String {
        self.input_entity(slot)
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// A field edit (`submit` is `None`) or Enter (`Some(shift)`).
    fn on_input(
        &mut self,
        slot: InputSlot,
        value: String,
        submit: Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (slot, submit) {
            (InputSlot::Find, None) => {
                let case = self.find.as_ref().is_some_and(|find| find.case_sensitive);
                self.run_search(value, case, cx);
            }
            (InputSlot::Find, Some(shift)) => {
                self.step_find(if shift { -1 } else { 1 }, window, cx)
            }
            (InputSlot::Rename, Some(_)) => self.apply_rename(value, window, cx),
            (InputSlot::SessionName | InputSlot::SessionDir, Some(_)) => {
                let name = self.input_value(InputSlot::SessionName, cx);
                let directory = self
                    .input_entity(InputSlot::SessionDir)
                    .map(|input| input.read(cx).value().to_string());
                self.apply_session(name, directory, window, cx);
            }
            (InputSlot::HostTarget | InputSlot::HostName, Some(_)) => {
                let target = self.input_value(InputSlot::HostTarget, cx);
                let name = self.input_value(InputSlot::HostName, cx);
                self.apply_add_host(target, name, window, cx);
            }
            _ => {}
        }
        cx.notify();
    }

    /// The palette query changed. In Go To, a leading `>` switches to commands
    /// and keeps the rest of the query; the path picker searches the host.
    pub(super) fn palette_query(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.modal {
            Modal::Commands | Modal::Goto if query.starts_with('!') => {
                let text = query[1..].trim_start();
                if !text.is_empty() {
                    self.capture.draft = text.to_owned();
                }
                self.open_modal(Modal::Capture, window, cx);
            }
            Modal::Goto if query.starts_with('>') => {
                self.modal = Modal::Commands;
                self.mark_prepared();
                let rest = query.trim_start_matches('>').trim_start().to_owned();
                if let Some(state) = self.palette_state.clone() {
                    state.update(cx, |state, cx| state.set_query(rest, window, cx));
                }
            }
            Modal::Path => {
                let Some(picker) = self.picker.as_ref() else {
                    return;
                };
                if picker.query != query {
                    let root = picker.root.clone();
                    self.query_paths(root, query.to_owned());
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// Narrow Go To by agent state. The typed query stays, and the highlight
    /// follows the same entry when it is still listed. This does not open,
    /// resize, focus, acknowledge a terminal, or record a visit.
    pub(crate) fn set_goto_filter(
        &mut self,
        filter: goto_filter::GotoFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal != Modal::Goto || self.goto.filter == filter {
            return;
        }
        let Some(state) = self.palette_state.clone() else {
            return;
        };
        let query = state.read(cx).query(cx).to_string();
        prepare_goto(self, &query);
        let index = state.read(cx).selected_index();
        let selected = index.and_then(|index| {
            self.goto_search
                .ranked()
                .get(index.section)
                .and_then(|section| section.rows.get(index.row))
                .and_then(goto_id)
        });
        let entries = self.goto_search.candidates();
        self.goto.selected = selected;
        let mut cursor = goto_filter::GotoCursor {
            query,
            selected: self.goto.selected.clone(),
        };
        goto_filter::apply_filter(&mut cursor, &entries, filter);
        self.goto.filter = filter;
        self.goto.selected = cursor.selected;
        self.goto.align = true;
        state.update(cx, |state, cx| state.focus(window, cx));
        cx.notify();
    }

    /// Move the Go To highlight onto the entry a filter change kept.
    pub(super) fn align_goto_highlight(
        &mut self,
        sections: &[Section],
        state: &Entity<CommandState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal != Modal::Goto || !self.goto.align {
            return;
        }
        let ids = section_ids(sections);
        let path = goto_filter::selection_path(&ids, self.goto.selected.as_ref());
        self.goto.selected = path.and_then(|(section, row)| ids.get(section)?.get(row).cloned());
        let desired = path.map(|(section, row)| IndexPath::new(row).section(section));
        if state.read(cx).selected_index() == desired {
            self.goto.align = false;
            return;
        }
        let state = state.clone();
        let desk = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            let applied = state.update(cx, |state, cx| {
                state.set_selected_index(desired, window, cx);
                state.selected_index() == desired
            });
            if applied {
                let _ = desk.update(cx, |desk, _| desk.goto.align = false);
            }
        });
    }

    /// Open Settings directly on `page`, replacing any open modal.
    pub(crate) fn show_settings_page(
        &mut self,
        page: crate::app::settings_panel::Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.modal = Modal::Settings;
        self.settings_page = Some(page);
        self.mark_prepared();
        crate::app::settings_panel::prepare(self, window, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Run a confirmed palette row. Commands already ran through their [`Run`] action.
    /// A Go To pane or tab is remembered only when that destination is still listed.
    pub(super) fn pick(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.note_goto(&target);
        match target {
            Target::Command => {}
            Target::Pane(terminal) => {
                self.close_modal(window, cx);
                self.reveal_terminal(&terminal);
                self.after_command(window, cx);
            }
            Target::Tab(tab) => {
                self.close_modal(window, cx);
                self.select_tab(&tab);
                self.after_command(window, cx);
            }
            Target::Session(name) => self.open_session_window(name, window, cx),
            Target::Window { handle, desk, tab } => {
                self.close_modal(window, cx);
                if desk.entity_id() == cx.entity_id() {
                    if let Some(tab) = tab {
                        self.select_tab(&tab);
                    }
                    self.after_command(window, cx);
                    return;
                }
                self.after_command(window, cx);
                // Another window can be updated once this window's lease ends.
                cx.defer(move |cx| raise_window(handle, &desk, tab.as_deref(), cx));
            }
            Target::Work(destination) => {
                self.close_modal(window, cx);
                self.navigate_work(destination, cx);
                self.after_command(window, cx);
            }
            Target::Setting(page) => self.show_settings_page(page, window, cx),
            Target::Move(key, name) => {
                let selected = self.organization.selected.clone();
                self.move_resources_to(selected, key, name, window, cx);
            }
            Target::Path(item) => match item {
                PathItem::Parent(directory) => self.browse_path(directory, window, cx),
                PathItem::Row { path, .. } => self.insert_path(path, window, cx),
            },
        }
    }

    /// Record a Go To open. A missing pane or tab, and every other row, is ignored.
    fn note_goto(&mut self, target: &Target) {
        let now = goto_frecency::now();
        let Some(link) = self.link.as_ref() else {
            return;
        };
        let boot = link.connection.server_id();
        let key = match target {
            Target::Pane(id) => {
                let listed = link.connection.pane(id).is_some()
                    || link.workspace.tabs.iter().any(|tab| {
                        crate::workspace::layout::placements(&tab.root)
                            .iter()
                            .any(|placement| placement.terminal_id.as_ref() == Some(id))
                    });
                let spelling = id.to_string();
                goto_frecency::validated_key(
                    boot.as_deref(),
                    goto_frecency::Choice::Pane {
                        id: &spelling,
                        listed,
                    },
                )
            }
            Target::Tab(id) => {
                let listed = link.workspace.tabs.iter().any(|tab| tab.id == *id);
                goto_frecency::validated_key(
                    boot.as_deref(),
                    goto_frecency::Choice::Tab { id, listed },
                )
            }
            _ => None,
        };
        self.goto_frecency.note(key, now);
    }
}
